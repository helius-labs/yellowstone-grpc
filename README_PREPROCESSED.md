# Rust client for Solana gRPC interface - Preprocessed Transactions

This functionality only exists in this feature branch but we aim to merge it soon to the primary Yellowstone repo.

## Prerequisites

To use this library, you need to use the `preprocessed-client-support` branch from the Helius Labs repository. Add the following to your `Cargo.toml`:

```toml
[dependencies]
yellowstone-grpc-client = { git = "https://github.com/helius-labs/yellowstone-grpc.git", branch = "preprocessed-client-support" }
yellowstone-grpc-proto = { git = "https://github.com/helius-labs/yellowstone-grpc.git", branch = "preprocessed-client-support", default-features = false }
```

## Configuration

- **Endpoint**: Use one of the Laserstream endpoints, for example:
  - `https://laserstream-mainnet-ewr.helius-rpc.com`
  - Other Laserstream endpoints may be available in different regions

- **X-Token**: Your Helius API key. This is required for authentication.

## Running Preprocessed Transactions

### Basic Example

```shell
cargo run --bin subscribe-preprocessed -- \
  --endpoint "https://laserstream-mainnet-ewr.helius-rpc.com" \
  --x-token "<your-helius-api-key>"
```

### What the Example Does

The `subscribe-preprocessed` binary:
1. Connects to the Laserstream endpoint using your Helius API key
2. Subscribes to preprocessed transaction updates
3. Filters out vote transactions by default
4. Sends periodic ping messages every 3 seconds to keep the connection alive
5. Receives and logs:
   - Preprocessed transactions
   - Ping messages from the server
   - Pong responses with IDs

### Example Output

```
transaction received: <transaction data>
ping received
pong received: id#1
transaction received: <transaction data>
pong received: id#2
```

## Complete Code Example

Here is the complete source code for `subscribe-preprocessed.rs`:

```rust
use {
    clap::Parser,
    futures::{sink::SinkExt, stream::StreamExt},
    log::info,
    std::env,
    tokio::time::{Duration, interval},
    tonic::transport::channel::ClientTlsConfig,
    yellowstone_grpc_client::GeyserGrpcClient,
    yellowstone_grpc_proto::{geyser::{SubscribePreprocessedRequest, SubscribePreprocessedRequestFilterTransactions, SubscribePreprocessedTransaction, SubscribeUpdatePong}, prelude::{
        SubscribeRequestPing, subscribe_preprocessed_update::UpdateOneof
    }},
};

#[derive(Debug, Clone, Parser)]
#[clap(author, version, about)]
struct Args {
    /// Service endpoint
    #[clap(short, long)]
    endpoint: String,

    #[clap(long)]
    x_token: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env::set_var(
        env_logger::DEFAULT_FILTER_ENV,
        env::var_os(env_logger::DEFAULT_FILTER_ENV).unwrap_or_else(|| "info".into()),
    );
    env_logger::init();

    let args = Args::parse();

    let mut client = GeyserGrpcClient::build_from_shared(args.endpoint)?
        .x_token(Some(args.x_token))?
        .tls_config(ClientTlsConfig::new().with_native_roots())?
        .connect()
        .await?;
    let (mut subscribe_tx, mut stream) = client.subscribe_preprocessed().await?;

    futures::try_join!(
        async move {
            subscribe_tx
                .send(SubscribePreprocessedRequest {
                    transactions: maplit::hashmap! {
                        "".to_owned() => SubscribePreprocessedRequestFilterTransactions {
                            vote: Some(false),
                            ..Default::default()
                        }
                    },
                    ..Default::default()
                })
                .await?;

            let mut timer = interval(Duration::from_secs(3));
            let mut id = 0;
            loop {
                timer.tick().await;
                id += 1;
                subscribe_tx
                    .send(SubscribePreprocessedRequest {
                        ping: Some(SubscribeRequestPing { id }),
                        ..Default::default()
                    })
                    .await?;
            }
            #[allow(unreachable_code)]
            Ok::<(), anyhow::Error>(())
        },
        async move {
            while let Some(message) = stream.next().await {
                match message?.update_oneof.expect("valid message") {
                    UpdateOneof::Transaction(SubscribePreprocessedTransaction { transaction, .. }) => {
                        info!("transaction received: {transaction:?}");
                    }
                    UpdateOneof::Ping(_msg) => {
                        info!("ping received");
                    }
                    UpdateOneof::Pong(SubscribeUpdatePong { id }) => {
                        info!("pong received: id#{id}");
                    }
                }
            }
            Ok::<(), anyhow::Error>(())
        }
    )?;

    Ok(())
}
```

## Customizing Filters

You can modify the transaction filters in the code. The example currently filters out vote transactions:

```rust
SubscribePreprocessedRequestFilterTransactions {
    vote: Some(false),
    ..Default::default()
}
```

You can adjust other filter options such as:
- `failed`: Filter failed transactions
- `account_include`: Include transactions involving specific accounts
- `account_exclude`: Exclude transactions involving specific accounts
- `account_required`: Require all specified accounts to be present

## Notes

- Ping messages are automatically sent every 3 seconds to maintain the connection
- The stream will continue until interrupted (Ctrl+C)

