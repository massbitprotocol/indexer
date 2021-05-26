// use error_chain::error_chain;
use serde::Deserialize;
use serde_json::json;
use std::env;
use reqwest::Client;
use std::error::Error;
use tokio_postgres::{NoTls};

#[derive(Deserialize, Debug)]
struct BlockResult {
    id: u64,
    jsonrpc: String,
    result: String,
}

#[tokio::main]
async fn main() ->  Result<(), Box<dyn Error>> {
    let (client, connection) =
        tokio_postgres::connect("postgresql://graph-node:let-me-in@localhost:5432/test", NoTls).await?;

    // Hard-code the latest block number here
    for i in 0..5000 {
        let gist_body = json!({
            "jsonrpc": "2.0",
            "method": "chain_getBlockHash",
            "params": [i],
            "id": 1
            });
    
        let request_url = "http://localhost:9933";
        let response = Client::new()
            .post(request_url)
            .json(&gist_body)
            .send().await?;
    
        let blockResult: BlockResult = response.json().await?;
        println!("{:?}", blockResult);

        let hash = blockResult.result;
        // let timestamp = None::<&[u8]>;
        // client.execute(
        //     "INSERT INTO graph (hash, timestamp) VALUES ($1, $2)",
        //     &[&name, &data],
        // )?;
    }
    Ok(())
}