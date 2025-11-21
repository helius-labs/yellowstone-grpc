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
