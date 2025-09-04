use {
    anyhow::Result,
    clap::Parser,
    log::{error, info},
    std::{
        sync::{Arc, RwLock},
        time::Duration,
    },
    tokio::{signal, sync::mpsc},
    yellowstone_grpc_geyser::{config::Config, grpc::GrpcService, metrics::PrometheusService},
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

    // Initialize Clickhouse if configured
    if let Some(clickhouse_config) = config.clickhouse {
        clickhouse_sink::init(clickhouse_config)
            .await
            .expect("Failed to setup clickhouse");
        info!("Clickhouse sink initialized");
    }

    // Create debug client channels
    let (debug_client_tx, debug_client_rx) = mpsc::unbounded_channel();

    // Store address before moving config
    let grpc_address = config.grpc.address;
    let prometheus_address = config.prometheus.as_ref().map(|p| p.address);

    // Create shared raw client channels
    let raw_client_channels = Arc::new(RwLock::new(Vec::new()));

    // Create gRPC service
    let (snapshot_channel, grpc_channel, grpc_shutdown) = GrpcService::create(
        config.tokio,
        config.grpc,
        config.debug_clients_http.then_some(debug_client_tx),
        raw_client_channels.clone(),
        false, // is_reload = false for standalone server
    )
    .await?;

    // Create Prometheus service
    let prometheus = PrometheusService::new(
        config.prometheus,
        config.debug_clients_http.then_some(debug_client_rx),
    )
    .await?;

    info!("gRPC server started on address: {}", grpc_address);

    if let Some(prometheus_addr) = prometheus_address {
        info!("Prometheus metrics available at: {}", prometheus_addr);
    }

    // Optional test mode - simulate some messages
    if args.test_mode {
        info!("Running in test mode - simulating messages");
        tokio::spawn(simulate_messages(
            grpc_channel.clone(),
            raw_client_channels.clone(),
        ));
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
    drop(grpc_channel);
    prometheus.shutdown();

    // Close snapshot channel if it exists
    drop(snapshot_channel);

    info!("Server shutdown complete");
    Ok(())
}

#[allow(unused)]
async fn simulate_messages(
    grpc_channel: mpsc::UnboundedSender<yellowstone_grpc_proto::plugin::message::Message>,
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
        log::warn,
        prost_types::Timestamp,
        std::time::SystemTime,
        yellowstone_grpc_proto::plugin::message::{Message, MessageSlot, SlotStatus},
    };

    let mut slot = 1000u64;
    let mut interval = tokio::time::interval(Duration::from_secs(1));

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

        // Send to raw clients first (same logic as plugin.rs)
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

        // Then send to regular geyser_loop pipeline
        if grpc_channel.send(message).is_err() {
            break;
        }

        slot += 1;

        if slot % 10 == 0 {
            info!("Simulated slot: {}", slot);
        }
    }
}
