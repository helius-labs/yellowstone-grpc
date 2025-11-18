use {
    anyhow::Result,
    clap::Parser,
    log::{error, info},
    std::{
        sync::{Arc, RwLock},
        time::Duration,
    },
    tokio::signal,
    yellowstone_grpc_geyser::{config::Config, grpc::GrpcService},
};

#[derive(Debug, Parser)]
#[clap(author, version, about)]
struct Args {
    #[clap(short, long, default_value = "config.json")]
    /// Path to the configuration file
    config: String,

    #[clap(long)]
    /// Enable test mode with simulated messages
    test_mode: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Load configuration
    let config = Config::load_from_file(&args.config)
        .map_err(|e| anyhow::anyhow!("Failed to load config: {}", e))?;

    // Setup logger
    solana_logger::setup_with_default(&config.log.level);
    info!("Starting Yellowstone gRPC server...");

    // Store address before moving config
    let grpc_address = config.grpc.address;

    // Create shared raw client channels
    let raw_client_channels = Arc::new(RwLock::new(Vec::new()));

    // Create gRPC service
    let grpc_shutdown = GrpcService::create(
        config.grpc,
        raw_client_channels.clone(),
        false, // is_reload = false for standalone server
    )
    .await?;

    info!("gRPC server started on address: {}", grpc_address);

    // Optional test mode - simulate some messages
    if args.test_mode {
        info!("Running in test mode - simulating messages");
        tokio::spawn(simulate_messages(raw_client_channels.clone()));
    }

    // Wait for shutdown signal
    match signal::ctrl_c().await {
        Ok(()) => {
            info!("Received Ctrl+C, shutting down gracefully...");
        }
        Err(err) => {
            error!("Unable to listen for shutdown signal: {}", err);
        }
    }

    // Shutdown services
    grpc_shutdown.notify_one();

    info!("Server shutdown complete");
    Ok(())
}

async fn simulate_messages(
    raw_client_channels: Arc<
        RwLock<
            Vec<(
                u64,
                crossbeam_channel::Sender<yellowstone_grpc_proto::plugin::message::Message>,
            )>,
        >,
    >,
) {
    use {
        log::{info, warn},
        prost_types::Timestamp,
        std::time::SystemTime,
        yellowstone_grpc_proto::plugin::message::{Message, MessageSlot, SlotStatus},
    };

    let mut slot = 1000u64;
    let mut interval = tokio::time::interval(Duration::from_micros(100));

    loop {
        interval.tick().await;

        // Simulate a processed slot message
        let message = Message::Slot(MessageSlot {
            slot,
            parent: Some(slot - 1),
            status: SlotStatus::Processed,
            dead_error: None,
            created_at: Timestamp::from(SystemTime::now()),
        });

        // Send to raw clients
        if let Ok(raw_clients) = raw_client_channels.read() {
            if !raw_clients.is_empty() {
                for (id, tx) in raw_clients.iter() {
                    if let Err(_) = tx.send(message.clone()) {
                        // Channel disconnected, will be cleaned up later
                        warn!("Raw client {} channel disconnected", id);
                    }
                }
            }
        }

        slot += 1;

        if slot % 1000 == 0 {
            info!("Simulated slot: {}", slot);
        }
    }
}
