use {
    anyhow::Result,
    clap::Parser,
    log::{error, info},
    std::{collections::HashMap, thread, time::Duration},
    tokio::signal,
    yellowstone_grpc_geyser::{
        config::Config,
        grpc::{ClientCommand, GrpcService},
    },
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

    // Create command channel with crossbeam (bounded 1M)
    let (client_command_tx, client_command_rx) = crossbeam_channel::bounded(1_000_000);

    // Spawn dedicated broadcaster thread (std::thread)
    let broadcast_client_command_tx = client_command_tx.clone();
    thread::spawn(move || {
        let mut clients: HashMap<
            u64,
            tokio::sync::mpsc::Sender<yellowstone_grpc_proto::plugin::message::Message>,
        > = HashMap::new();

        loop {
            match client_command_rx.recv() {
                Ok(ClientCommand::Subscribe { client_id, sender }) => {
                    clients.insert(client_id, sender);
                    info!(
                        "Client {} subscribed, total clients: {}",
                        client_id,
                        clients.len()
                    );
                }
                Ok(ClientCommand::Unsubscribe { client_id }) => {
                    clients.remove(&client_id);
                    info!(
                        "Client {} unsubscribed, remaining clients: {}",
                        client_id,
                        clients.len()
                    );
                }
                Ok(ClientCommand::Broadcast { message }) => {
                    // Remove disconnected clients while broadcasting
                    clients.retain(|id, tx| {
                        if tx.try_send(message.clone()).is_err() {
                            log::warn!("Client {} channel disconnected during broadcast", id);
                            false
                        } else {
                            true
                        }
                    });
                }
                Err(_) => break,
            }
        }
    });

    // Create gRPC service
    let grpc_shutdown = GrpcService::create(
        config.grpc,
        broadcast_client_command_tx,
        false, // is_reload = false for standalone server
    )
    .await?;

    info!("gRPC server started on address: {}", grpc_address);

    // Optional test mode - simulate some messages
    if args.test_mode {
        info!("Running in test mode - simulating messages");
        thread::spawn(move || simulate_messages(client_command_tx.clone()));
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

fn simulate_messages(client_command_tx: crossbeam_channel::Sender<ClientCommand>) {
    use {
        log::info,
        prost_types::Timestamp,
        std::{thread, time::SystemTime},
        yellowstone_grpc_proto::plugin::message::{Message, MessageSlot, SlotStatus},
    };

    let mut slot = 1000u64;
    let interval = Duration::from_micros(100);

    loop {
        thread::sleep(interval);

        // Simulate a processed slot message
        let message = Message::Slot(MessageSlot {
            slot,
            parent: Some(slot - 1),
            status: SlotStatus::Processed,
            dead_error: None,
            created_at: Timestamp::from(SystemTime::now()),
        });

        // Send broadcast command
        let _ = client_command_tx.send(ClientCommand::Broadcast { message });

        slot += 1;

        if slot % 1000 == 0 {
            info!("Simulated slot: {}", slot);
        }
    }
}
