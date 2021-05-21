use ethereum::{EthereumNetworks, ProviderEthRpcMetrics};
use futures::future::join_all;
use git_testament::{git_testament};
use graph::{prometheus::Registry};
use lazy_static::lazy_static;
use std::str::FromStr;
use std::time::Duration;
use std::{collections::HashMap, env};
use structopt::StructOpt;

use graph::blockchain::block_ingestor::BlockIngestor;
use graph::blockchain::Blockchain as _;
use graph::components::{store::BlockStore};
use graph::log::logger;
use graph::prelude::{IndexNodeServer as _, JsonRpcServer as _, *};
use graph_chain_ethereum::{self as ethereum, EthereumAdapterTrait, Transport};
use graph_core::{
    MetricsRegistry,
};
use graph_store_postgres::{register_jobs as register_store_jobs, ChainHeadUpdateListener, Store};

mod config;
mod opt;
mod store_builder;

use config::Config;
use store_builder::StoreBuilder;

lazy_static! {
    // Default to an Ethereum reorg threshold to 50 blocks
    static ref REORG_THRESHOLD: BlockNumber = env::var("ETHEREUM_REORG_THRESHOLD")
        .ok()
        .map(|s| BlockNumber::from_str(&s)
            .unwrap_or_else(|_| panic!("failed to parse env var ETHEREUM_REORG_THRESHOLD")))
        .unwrap_or(50);

    // Default to an ancestor count of 50 blocks
    static ref ANCESTOR_COUNT: BlockNumber = env::var("ETHEREUM_ANCESTOR_COUNT")
        .ok()
        .map(|s| BlockNumber::from_str(&s)
             .unwrap_or_else(|_| panic!("failed to parse env var ETHEREUM_ANCESTOR_COUNT")))
        .unwrap_or(50);
}

/// How long we will hold up node startup to get the net version and genesis
/// hash from the client. If we can't get it within that time, we'll try and
/// continue regardless.
const ETH_NET_VERSION_WAIT_TIME: Duration = Duration::from_secs(30);

git_testament!(TESTAMENT);

#[tokio::main]
async fn main() {
    let opt = opt::Opt::from_args();

    // Set up logger
    let logger = logger(opt.debug);

    let config = match Config::load(&logger, &opt.clone().into()) {
        Err(e) => {
            eprintln!("configuration error: {}", e);
            std::process::exit(1);
        }
        Ok(config) => config,
    };

    let node_id =
        NodeId::new(opt.node_id.clone()).expect("Node ID must contain only a-z, A-Z, 0-9, and '_'");
    let query_only = config.query_only(&node_id);

    // Optionally, identify the Elasticsearch logging configuration
    let elastic_config = opt
        .elasticsearch_url
        .clone()
        .map(|endpoint| ElasticLoggingConfig {
            endpoint: endpoint.clone(),
            username: opt.elasticsearch_user.clone(),
            password: opt.elasticsearch_password.clone(),
        });

    // Create a component and subgraph logger factory
    let logger_factory = LoggerFactory::new(logger.clone(), elastic_config);

    //  Set up Prometheus registry
    let prometheus_registry = Arc::new(Registry::new());
    let metrics_registry = Arc::new(MetricsRegistry::new(
        logger.clone(),
        prometheus_registry.clone(),
    ));

    let eth_networks = if query_only {
        EthereumNetworks::new()
    } else {
        create_ethereum_networks(logger.clone(), metrics_registry.clone(), config.clone())
            .await
            .expect("Failed to parse Ethereum networks")
    };

    let store_builder =
        StoreBuilder::new(&logger, &node_id, &config, metrics_registry.cheap_clone()).await;

    let launch_services = |logger: Logger| async move {
        let (eth_networks, idents) = connect_networks(&logger, eth_networks).await;

        let chain_head_update_listener = store_builder.chain_head_update_listener();
        let network_store = store_builder.network_store(idents);
        
        let chains = Arc::new(networks_as_chains(
            &logger,
            node_id.clone(),
            metrics_registry.clone(),
            &eth_networks,
            network_store.as_ref(),
            chain_head_update_listener.clone(),
            &logger_factory,
        ));

        // let block_polling_interval = Duration::from_millis(opt.ethereum_polling_interval);
        let block_polling_interval = Duration::from_secs(3);

        start_block_ingestor(&logger, block_polling_interval, &chains);

        // Start a task runner
        let mut job_runner = graph::util::jobs::Runner::new(&logger);
        register_store_jobs(&mut job_runner, network_store.clone());
        graph::spawn_blocking(job_runner.start());
    };

    graph::spawn(launch_services(logger.clone()));
    futures::future::pending::<()>().await;
}

/// Parses an Ethereum connection string and returns the network name and Ethereum adapter.
async fn create_ethereum_networks(
    logger: Logger,
    registry: Arc<MetricsRegistry>,
    config: Config,
) -> Result<EthereumNetworks, anyhow::Error> {
    let eth_rpc_metrics = Arc::new(ProviderEthRpcMetrics::new(registry));
    let mut parsed_networks = EthereumNetworks::new();
    for (name, chain) in config.chains.chains {
        for provider in chain.providers {
            let capabilities = provider.node_capabilities();

            let logger = logger.new(o!("provider" => provider.label.clone()));
            info!(
                logger,
                "Creating transport";
                "url" => &provider.url,
                "capabilities" => capabilities
            );

            use crate::config::Transport::*;

            let (transport_event_loop, transport) = match provider.transport {
                Rpc => Transport::new_rpc(&provider.url),
                Ipc => Transport::new_ipc(&provider.url),
                Ws => Transport::new_ws(&provider.url),
            };

            // If we drop the event loop the transport will stop working.
            // For now it's fine to just leak it.
            std::mem::forget(transport_event_loop);

            let supports_eip_1898 = !provider.features.contains("no_eip1898");

            parsed_networks.insert(
                name.to_string(),
                capabilities,
                Arc::new(
                    graph_chain_ethereum::EthereumAdapter::new(
                        logger,
                        provider.label,
                        &provider.url,
                        transport,
                        eth_rpc_metrics.clone(),
                        supports_eip_1898,
                    )
                    .await,
                ),
            );
        }
    }
    parsed_networks.sort();
    Ok(parsed_networks)
}

/// Try to connect to all the providers in `eth_networks` and get their net
/// version and genesis block. Return the same `eth_networks` and the
/// retrieved net identifiers grouped by network name. Remove all providers
/// for which trying to connect resulted in an error from the returned
/// `EthereumNetworks`, since it's likely pointless to try and connect to
/// them. If the connection attempt to a provider times out after
/// `ETH_NET_VERSION_WAIT_TIME`, keep the provider, but don't report a
/// version for it.
async fn connect_networks(
    logger: &Logger,
    mut eth_networks: EthereumNetworks,
) -> (
    EthereumNetworks,
    Vec<(String, Vec<EthereumNetworkIdentifier>)>,
) {
    // The status of a provider that we learned from connecting to it
    #[derive(PartialEq)]
    enum Status {
        Broken {
            network: String,
            provider: String,
        },
        Version {
            network: String,
            ident: EthereumNetworkIdentifier,
        },
    }

    // This has one entry for each provider, and therefore multiple entries
    // for each network
    let statuses = join_all(
        eth_networks
            .flatten()
            .into_iter()
            .map(|(network_name, capabilities, eth_adapter)| {
                (network_name, capabilities, eth_adapter, logger.clone())
            })
            .map(|(network, capabilities, eth_adapter, logger)| async move {
                let logger = logger.new(o!("provider" => eth_adapter.provider().to_string()));
                info!(
                    logger, "Connecting to Ethereum to get network identifier";
                    "capabilities" => &capabilities
                );
                match tokio::time::timeout(ETH_NET_VERSION_WAIT_TIME, eth_adapter.net_identifiers())
                    .await
                    .map_err(Error::from)
                {
                    // An `Err` means a timeout, an `Ok(Err)` means some other error (maybe a typo
                    // on the URL)
                    Ok(Err(e)) | Err(e) => {
                        error!(logger, "Connection to provider failed. Not using this provider";
                                       "error" =>  e.to_string());
                        Status::Broken {
                            network,
                            provider: eth_adapter.provider().to_string(),
                        }
                    }
                    Ok(Ok(ident)) => {
                        info!(
                            logger,
                            "Connected to Ethereum";
                            "network_version" => &ident.net_version,
                            "capabilities" => &capabilities
                        );
                        Status::Version { network, ident }
                    }
                }
            }),
    )
    .await;

    // Group identifiers by network name
    let idents: HashMap<String, Vec<EthereumNetworkIdentifier>> =
        statuses
            .into_iter()
            .fold(HashMap::new(), |mut networks, status| {
                match status {
                    Status::Broken { network, provider } => {
                        eth_networks.remove(&network, &provider)
                    }
                    Status::Version { network, ident } => {
                        networks.entry(network.to_string()).or_default().push(ident)
                    }
                }
                networks
            });
    let idents: Vec<_> = idents.into_iter().collect();
    (eth_networks, idents)
}


fn networks_as_chains(
    logger: &Logger,
    node_id: NodeId,
    registry: Arc<MetricsRegistry>,
    eth_networks: &EthereumNetworks,
    store: &Store,
    chain_head_update_listener: Arc<ChainHeadUpdateListener>,
    logger_factory: &LoggerFactory,
) -> HashMap<String, Arc<ethereum::Chain>> {
    let chains = eth_networks
        .networks
        .iter()
        .filter_map(|(network_name, eth_adapters)| {
            store
                .block_store()
                .chain_store(network_name)
                .map(|chain_store| {
                    let is_ingestible = chain_store.is_ingestible();
                    (network_name, eth_adapters, chain_store, is_ingestible)
                })
                .or_else(|| {
                    error!(
                        logger,
                        "No store configured for chain {}; ignoring this chain", network_name
                    );
                    None
                })
        })
        .map(|(network_name, eth_adapters, chain_store, is_ingestible)| {
            let chain = ethereum::Chain::new(
                logger_factory.clone(),
                network_name.clone(),
                node_id.clone(),
                registry.clone(),
                chain_store,
                store.subgraph_store(),
                eth_adapters.clone(),
                chain_head_update_listener.clone(),
                *ANCESTOR_COUNT,
                *REORG_THRESHOLD,
                is_ingestible,
            );
            (network_name.clone(), Arc::new(chain))
        });
    HashMap::from_iter(chains)
}

fn start_block_ingestor(
    logger: &Logger,
    block_polling_interval: Duration,
    chains: &HashMap<String, Arc<ethereum::Chain>>,
) {
    // BlockIngestor must be configured to keep at least REORG_THRESHOLD ancestors,
    // otherwise BlockStream will not work properly.
    // BlockStream expects the blocks after the reorg threshold to be present in the
    // database.
    assert!(*ANCESTOR_COUNT >= *REORG_THRESHOLD);

    info!(logger, "Starting block ingestors");

    // Create Ethereum block ingestors and spawn a thread to run each
    chains
        .iter()
        .filter(|(network_name, chain)| {
            if !chain.is_ingestible {
                error!(logger, "Not starting block ingestor (chain is defective)"; "network_name" => &network_name);
            }
            chain.is_ingestible
        })
        .for_each(|(network_name, chain)| {
            info!(
                logger,
                "Starting block ingestor for network";
                "network_name" => &network_name
            );

            let block_ingestor = BlockIngestor::<ethereum::Chain>::new(
                chain.ingestor_adapter(),
                block_polling_interval,
            )
            .expect("failed to create Ethereum block ingestor");

            // Run the Ethereum block ingestor in the background
            graph::spawn(block_ingestor.into_polling_stream());
        });
}

#[cfg(test)]
mod test {
    use super::create_ethereum_networks;
    use crate::config::{Config, Opt};
    use graph::components::ethereum::NodeCapabilities;
    use graph::log::logger;
    use graph::prelude::tokio;
    use graph::prometheus::Registry;
    use graph_core::MetricsRegistry;
    use std::sync::Arc;

    #[tokio::test]
    async fn correctly_parse_ethereum_networks() {
        let logger = logger(true);

        let network_args = vec![
            "mainnet:traces:http://localhost:8545/".to_string(),
            "goerli:archive:http://localhost:8546/".to_string(),
        ];

        let opt = Opt {
            postgres_url: Some("not needed".to_string()),
            config: None,
            store_connection_pool_size: 5,
            postgres_secondary_hosts: vec![],
            postgres_host_weights: vec![],
            disable_block_ingestor: true,
            node_id: "default".to_string(),
            ethereum_rpc: network_args,
            ethereum_ws: vec![],
            ethereum_ipc: vec![],
        };

        let config = Config::load(&logger, &opt).expect("can create config");
        let prometheus_registry = Arc::new(Registry::new());
        let metrics_registry = Arc::new(MetricsRegistry::new(
            logger.clone(),
            prometheus_registry.clone(),
        ));

        let ethereum_networks = create_ethereum_networks(logger, metrics_registry, config.clone())
            .await
            .expect("Correctly parse Ethereum network args");
        let mut network_names = ethereum_networks.networks.keys().collect::<Vec<&String>>();
        network_names.sort();

        let traces = NodeCapabilities {
            archive: false,
            traces: true,
        };
        let archive = NodeCapabilities {
            archive: true,
            traces: false,
        };
        let has_mainnet_with_traces = ethereum_networks
            .adapter_with_capabilities("mainnet".to_string(), &traces)
            .is_ok();
        let has_goerli_with_archive = ethereum_networks
            .adapter_with_capabilities("goerli".to_string(), &archive)
            .is_ok();
        let has_mainnet_with_archive = ethereum_networks
            .adapter_with_capabilities("mainnet".to_string(), &archive)
            .is_ok();
        let has_goerli_with_traces = ethereum_networks
            .adapter_with_capabilities("goerli".to_string(), &traces)
            .is_ok();

        assert_eq!(has_mainnet_with_traces, true);
        assert_eq!(has_goerli_with_archive, true);
        assert_eq!(has_mainnet_with_archive, false);
        assert_eq!(has_goerli_with_traces, false);

        let goerli_capability = ethereum_networks
            .networks
            .get("goerli")
            .unwrap()
            .adapters
            .iter()
            .next()
            .unwrap()
            .capabilities;
        let mainnet_capability = ethereum_networks
            .networks
            .get("mainnet")
            .unwrap()
            .adapters
            .iter()
            .next()
            .unwrap()
            .capabilities;
        assert_eq!(
            network_names,
            vec![&"goerli".to_string(), &"mainnet".to_string()]
        );
        assert_eq!(goerli_capability, archive);
        assert_eq!(mainnet_capability, traces);
    }
}
