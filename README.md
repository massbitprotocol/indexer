# Run the indexer
Start the graph node docker-compose with ipfs and postgres only, then run

```
cargo run -p graph-node --release -- \
  --postgres-url postgresql://graph-node:let-me-in@localhost:5432/graph-node \
  --ethereum-rpc mainnet:https://dev-gateway.massbit.io/bsc \
  --ipfs 127.0.0.1:5001
```

# Features
- Pooling from Ethereum (done)
- Calling Assemby code 
- ...
- Batch 