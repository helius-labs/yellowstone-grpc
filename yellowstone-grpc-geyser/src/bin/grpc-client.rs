use {
    anyhow::Result,
    clap::Parser,
    log::{error, info},
    std::time::{Duration, SystemTime},
    tokio::signal,
    tonic::transport::Channel,
    yellowstone_grpc_proto::{prelude::SubscribeRequest, tonic::Streaming},
};

#[derive(Debug, Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(short, long, default_value = "http://127.0.0.1:10000")]
    /// gRPC server endpoint
    endpoint: String,
}

#[derive(Debug)]
struct LatencyStats {
    latencies: Vec<Duration>,
    total_messages: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Setup logger
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("Starting Yellowstone gRPC client...");
    info!("Connecting to: {}", args.endpoint);

    // Connect to gRPC server
    let channel = Channel::from_shared(args.endpoint.clone())?
        .connect()
        .await?;

    let mut client =
        yellowstone_grpc_proto::prelude::geyser_client::GeyserClient::new(channel);

    // Subscribe to raw stream (empty request for raw mode)
    let request = tonic::Request::new(futures::stream::iter(vec![SubscribeRequest::default()]));

    info!("Subscribing to raw stream...");
    let response = client.subscribe_raw(request).await?;
    let mut stream: Streaming<_> = response.into_inner();

    info!("Connected! Measuring latencies...");

    let mut latencies = Vec::new();
    let mut total_messages = 0u64;

    loop {
        tokio::select! {
            message = stream.message() => {
                match message {
                    Ok(Some(update)) => {
                        total_messages += 1;

                        // Extract created_at timestamp from the update (not the inner message)
                        if let Some(timestamp) = update.created_at {
                            let created = SystemTime::UNIX_EPOCH
                                + Duration::from_secs(timestamp.seconds as u64)
                                + Duration::from_nanos(timestamp.nanos as u64);

                            let now = SystemTime::now();
                            if let Ok(latency) = now.duration_since(created) {
                                latencies.push(latency);
                            }
                        }

                        if total_messages % 1000 == 0 {
                            info!("Received {} messages", total_messages);
                        }
                    }
                    Ok(None) => {
                        info!("Stream ended");
                        break;
                    }
                    Err(e) => {
                        error!("Error receiving message: {}", e);
                        break;
                    }
                }
            }
            _ = signal::ctrl_c() => {
                info!("Received Ctrl+C, shutting down...");
                break;
            }
        }
    }

    // Print statistics
    print_statistics(LatencyStats {
        latencies,
        total_messages,
    });

    Ok(())
}

fn print_statistics(stats: LatencyStats) {
    info!("\n========== Latency Statistics ==========");
    info!("Total messages received: {}", stats.total_messages);

    if !stats.latencies.is_empty() {
        let mut sorted = stats.latencies.clone();
        sorted.sort();

        let p50 = sorted[sorted.len() * 50 / 100];
        let p90 = sorted[sorted.len() * 90 / 100];
        let p95 = sorted[sorted.len() * 95 / 100];
        let p99 = sorted[sorted.len() * 99 / 100];
        let max = sorted[sorted.len() - 1];
        let min = sorted[0];
        let avg = sorted.iter().sum::<Duration>() / sorted.len() as u32;

        info!("\nEnd-to-End Latency (created_at -> received):");
        info!("  Average: {:?}", avg);
        info!("  Min: {:?}", min);
        info!("  p50: {:?}", p50);
        info!("  p90: {:?}", p90);
        info!("  p95: {:?}", p95);
        info!("  p99: {:?}", p99);
        info!("  Max: {:?}", max);
    } else {
        info!("No latency measurements collected");
    }

    info!("==========================================\n");
}
