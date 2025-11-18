use {
    crate::{config::Config, grpc::GrpcService},
    agave_geyser_plugin_interface::geyser_plugin_interface::{
        GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions, ReplicaBlockInfoVersions,
        ReplicaEntryInfoVersions, ReplicaTransactionInfoVersions, Result as PluginResult,
        SlotStatus,
    },
    std::{
        concat, env,
        sync::{Arc, RwLock},
        time::Duration,
    },
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
    raw_client_channels: Arc<RwLock<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
}

impl PluginInner {
    fn send_message(&self, message: Message) {
        // Send to raw clients only
        if let Ok(raw_clients) = self.raw_client_channels.read() {
            if !raw_clients.is_empty() {
                for (id, tx) in raw_clients.iter() {
                    if let Err(_) = tx.send(message.clone()) {
                        // Channel disconnected, will be cleaned up later
                        log::warn!("Raw client {} channel disconnected", id);
                    }
                }
            }
        }
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

        let (grpc_shutdown, raw_client_channels) = runtime.block_on(async move {
            // Create shared raw client channels
            let raw_client_channels = Arc::new(RwLock::new(Vec::new()));

            let grpc_shutdown =
                GrpcService::create(config.grpc, raw_client_channels.clone(), is_reload)
                    .await
                    .map_err(|error| GeyserPluginError::Custom(format!("{error:?}").into()))?;

            Ok::<_, GeyserPluginError>((grpc_shutdown, raw_client_channels))
        })?;

        self.inner = Some(PluginInner {
            runtime,
            grpc_shutdown,
            raw_client_channels,
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
