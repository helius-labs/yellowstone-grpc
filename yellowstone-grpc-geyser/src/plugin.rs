use {
    crate::{
        config::Config,
        grpc::{ClientCommand, GrpcService},
    },
    agave_geyser_plugin_interface::geyser_plugin_interface::{
        GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions, ReplicaBlockInfoVersions,
        ReplicaEntryInfoVersions, ReplicaTransactionInfoVersions, Result as PluginResult,
        SlotStatus,
    },
    std::{collections::HashMap, concat, env, sync::Arc, thread, time::Duration},
    tokio::{
        runtime::{Builder, Runtime},
        sync::Notify,
    },
    yellowstone_grpc_proto::plugin::message::{Message, MessageTransaction},
};

#[derive(Debug)]
pub struct PluginInner {
    runtime: Runtime,
    grpc_shutdown: Arc<Notify>,
    client_command_tx: crossbeam_channel::Sender<ClientCommand>,
}

impl PluginInner {
    fn send_message(&self, message: Message) {
        // Send broadcast command to the dedicated thread
        let _ = self.client_command_tx.send(ClientCommand::Broadcast { message });
    }
}

#[derive(Debug, Default)]
pub struct Plugin {
    inner: Option<PluginInner>,
}

impl Plugin {
    fn with_inner<F>(&self, f: F) -> PluginResult<()>
    where
        F: FnOnce(&PluginInner) -> PluginResult<()>,
    {
        let inner = self.inner.as_ref().expect("initialized");
        f(inner)
    }
}

impl GeyserPlugin for Plugin {
    fn name(&self) -> &'static str {
        concat!(env!("CARGO_PKG_NAME"), "-", env!("CARGO_PKG_VERSION"))
    }

    fn on_load(&mut self, config_file: &str, is_reload: bool) -> PluginResult<()> {
        let config = Config::load_from_file(config_file)?;

        // Setup logger
        solana_logger::setup_with_default(&config.log.level);

        // Create inner
        let mut builder = Builder::new_multi_thread();
        if let Some(worker_threads) = config.tokio.worker_threads {
            builder.worker_threads(worker_threads);
        }
        if let Some(tokio_cpus) = config.tokio.affinity.clone() {
            builder.on_thread_start(move || {
                affinity_linux::set_thread_affinity(tokio_cpus.clone().into_iter())
                    .expect("failed to set affinity")
            });
        }
        let runtime = builder
            .thread_name_fn(crate::get_thread_name)
            .enable_all()
            .build()
            .map_err(|error| GeyserPluginError::Custom(Box::new(error)))?;

        let (grpc_shutdown, client_command_tx) = runtime.block_on(async move {
            // Create command channel with crossbeam
            let (client_command_tx, client_command_rx) = crossbeam_channel::unbounded();

            // Spawn dedicated broadcaster thread (std::thread)
            let broadcast_client_command_tx = client_command_tx.clone();
            thread::spawn(move || {
                let mut clients: HashMap<u64, tokio::sync::mpsc::UnboundedSender<Message>> = HashMap::new();

                while let Ok(command) = client_command_rx.recv() {
                    match command {
                        ClientCommand::Subscribe { client_id, sender } => {
                            clients.insert(client_id, sender);
                            log::info!(
                                "Client {} subscribed, total clients: {}",
                                client_id,
                                clients.len()
                            );
                        }
                        ClientCommand::Unsubscribe { client_id } => {
                            clients.remove(&client_id);
                            log::info!(
                                "Client {} unsubscribed, remaining clients: {}",
                                client_id,
                                clients.len()
                            );
                        }
                        ClientCommand::Broadcast { message } => {
                            // Remove disconnected clients while broadcasting
                            clients.retain(|id, tx| {
                                if tx.send(message.clone()).is_err() {
                                    log::warn!("Client {} channel disconnected during broadcast", id);
                                    false
                                } else {
                                    true
                                }
                            });
                        }
                    }
                }
            });

            let grpc_shutdown =
                GrpcService::create(config.grpc, broadcast_client_command_tx, is_reload)
                    .await
                    .map_err(|error| GeyserPluginError::Custom(format!("{error:?}").into()))?;

            Ok::<_, GeyserPluginError>((grpc_shutdown, client_command_tx))
        })?;

        self.inner = Some(PluginInner {
            runtime,
            grpc_shutdown,
            client_command_tx,
        });

        Ok(())
    }

    fn on_unload(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.grpc_shutdown.notify_one();
            inner.runtime.shutdown_timeout(Duration::from_secs(30));
        }
    }

    fn update_account(
        &self,
        _account: ReplicaAccountInfoVersions,
        _slot: u64,
        _is_startup: bool,
    ) -> PluginResult<()> {
        Ok(())
    }

    fn notify_end_of_startup(&self) -> PluginResult<()> {
        Ok(())
    }

    fn update_slot_status(
        &self,
        _slot: u64,
        _parent: Option<u64>,
        _status: &SlotStatus,
    ) -> PluginResult<()> {
        Ok(())
    }

    fn notify_transaction(
        &self,
        transaction: ReplicaTransactionInfoVersions<'_>,
        slot: u64,
    ) -> PluginResult<()> {
        self.with_inner(|inner| {
            let transaction = match transaction {
                ReplicaTransactionInfoVersions::V0_0_1(_info) => {
                    unreachable!("ReplicaAccountInfoVersions::V0_0_1 is not supported")
                }
                ReplicaTransactionInfoVersions::V0_0_2(info) => {
                    MessageTransaction::from_geyser(info, slot)
                }
                ReplicaTransactionInfoVersions::V0_0_3(info) => {
                    MessageTransaction::from_geyser_v3(info, slot)
                }
            };

            let message = Message::Transaction(transaction);
            inner.send_message(message);

            Ok(())
        })
    }

    fn notify_entry(&self, _entry: ReplicaEntryInfoVersions) -> PluginResult<()> {
        Ok(())
    }

    fn notify_block_metadata(&self, _blockinfo: ReplicaBlockInfoVersions<'_>) -> PluginResult<()> {
        Ok(())
    }

    fn account_data_notifications_enabled(&self) -> bool {
        false
    }

    fn account_data_snapshot_notifications_enabled(&self) -> bool {
        false
    }

    fn transaction_notifications_enabled(&self) -> bool {
        true
    }

    fn entry_notifications_enabled(&self) -> bool {
        false
    }
}

#[no_mangle]
#[allow(improper_ctypes_definitions)]
/// # Safety
///
/// This function returns the Plugin pointer as trait GeyserPlugin.
pub unsafe extern "C" fn _create_plugin() -> *mut dyn GeyserPlugin {
    let plugin = Plugin::default();
    let plugin: Box<dyn GeyserPlugin> = Box::new(plugin);
    Box::into_raw(plugin)
}
