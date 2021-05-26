## Note
Custom graph node will be migrated to https://github.com/massbitprotocol/graph-node

## Projects:
- Block Ingestor: ingest data from ethereum network (forked from graph-node)
- Custom Graph Node: to ingest data from substrate / solana (work-in-progress, forked from graph-node)
- indexer-from-scratch (Golang script to index substrate)
  - Go through all block and index timestamp (done)
  - Implement WASM Runtime (in-progress)
  - Go through all events (I think we need EVM integrated in Substrate) (in-progress)

## Prequisites

To run custom graph-node, make sure you have:
- Pulled https://github.com/massbitprotocol/rust-web3 into custom-graph-node folder
- cd into docker folder and run `docker-compose up`
- Then run `cargo run`
- Just in case, please install all of this
```shell
sudo apt install -y cmake pkg-config libssl-dev git gcc build-essential clang libclang-dev
```
  
- If you have "linking with cc failed", fix it by follow this issue https://github.com/filecoin-project/replication-game/issues/45
- To run cargo with bsc testnet, postgres (local) and ipfs (local), run:
```shell
cargo run -p graph-node --release --   --postgres-url postgresql://graph-node:let-me-in@localhost:5432/graph-node   --ethereum-rpc testnet:https://data-seed-prebsc-1-s1.binance.org:8545/   --ipfs 127.0.0.1:5001
```