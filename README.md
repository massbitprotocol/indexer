To run custom graph-node, make sure you have:
- Pulled https://github.com/massbitprotocol/rust-web3 into custom-graph-node folder
- cd into docker folder and run `docker-compose up`
- Then run `cargo run`
- If you have "linking with cc failed", fix it by follow this issue https://github.com/filecoin-project/replication-game/issues/45
- To run cargo with bsc testnet, postgres (local) and ipfs (local), run:
```shell
cargo run -p graph-node --release --   --postgres-url postgresql://graph-node:let-me-in@localhost:5432/graph-node   --ethereum-rpc testnet:https://data-seed-prebsc-1-s1.binance.org:8545/   --ipfs 127.0.0.1:5001
```
