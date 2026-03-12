use {
    crate::{
        config::Config,
        grpc::GrpcService,
        metrics::{self, PrometheusService},
    },
    agave_geyser_plugin_interface::geyser_plugin_interface::{
        GeyserPlugin, GeyserPluginError, ReplicaAccountInfoVersions, ReplicaBlockInfoVersions,
        ReplicaEntryInfoVersions, ReplicaTransactionInfoVersions, Result as PluginResult,
        SlotStatus,
    },
    solana_sdk::pubkey::Pubkey,
    std::{
        collections::HashMap,
        concat, env,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex, RwLock,
        },
        time::Duration,
    },
    tokio::{
        runtime::{Builder, Runtime},
        sync::{mpsc, Notify},
    },
    yellowstone_grpc_proto::plugin::message::{
        Message, MessageAccount, MessageBlockMeta, MessageEntry, MessageSlot, MessageTransaction,
    },
};

#[cfg(feature = "statsd")]
use ::metrics::set_global_recorder;
#[cfg(feature = "statsd")]
use metrics_exporter_statsd::StatsdBuilder;

/// Buffers large accounts keyed by slot, then by pubkey. Deduplicates rapid writes
/// within a slot by keeping only the highest write_version.
#[derive(Debug)]
struct AccountBufferState {
    size_threshold: usize,
    /// slot -> (pubkey -> account). Nested map gives O(1) slot lookup on flush/cleanup.
    slots: HashMap<u64, HashMap<Pubkey, MessageAccount>>,
}

impl AccountBufferState {
    fn new(size_threshold: usize) -> Self {
        Self {
            size_threshold,
            slots: HashMap::new(),
        }
    }

    /// Insert an account if it exceeds the size threshold. Returns true if buffered.
    fn try_insert(&mut self, account: MessageAccount) -> bool {
        if account.account.data.len() < self.size_threshold {
            return false;
        }

        let slot_map = self.slots.entry(account.slot).or_default();
        match slot_map.get(&account.account.pubkey) {
            Some(existing) if existing.account.write_version >= account.account.write_version => {}
            _ => {
                slot_map.insert(account.account.pubkey, account);
            }
        }
        true
    }

    /// Drain all entries for a specific slot. Returns them for sending.
    fn drain_slot(&mut self, slot: u64) -> Vec<MessageAccount> {
        self.slots
            .remove(&slot)
            .map(|m| m.into_values().collect())
            .unwrap_or_default()
    }

    /// Remove entries for all slots <= rooted_slot.
    fn cleanup(&mut self, rooted_slot: u64) {
        self.slots.retain(|s, _| *s > rooted_slot);
    }
}

#[derive(Debug)]
pub struct PluginInner {
    runtime: Runtime,
    snapshot_channel: Mutex<Option<crossbeam_channel::Sender<Box<Message>>>>,
    snapshot_channel_closed: AtomicBool,
    grpc_channel: mpsc::UnboundedSender<Message>,
    grpc_shutdown: Arc<Notify>,
    prometheus: PrometheusService,
    raw_client_channels: Arc<RwLock<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
    account_buffer: Option<Mutex<AccountBufferState>>,
}

impl PluginInner {
    fn send_message(&self, message: Message) {
        // Send to raw clients first (bypasses all processing)
        if let Ok(raw_clients) = self.raw_client_channels.read() {
            if !raw_clients.is_empty() {
                for (id, tx) in raw_clients.iter() {
                    if tx.send(message.clone()).is_err() {
                        // Channel disconnected, will be cleaned up later
                        log::warn!("Raw client {} channel disconnected", id);
                    }
                }
            }
        }

        // Then send to regular geyser_loop pipeline
        if self.grpc_channel.send(message).is_ok() {
            metrics::message_queue_size_inc();
        }
    }

    /// Try to buffer a large account. Returns true if buffered, false if buffering
    /// is disabled or the account is below the size threshold.
    fn try_buffer_account(&self, account: MessageAccount) -> bool {
        match &self.account_buffer {
            Some(buf) => buf.lock().unwrap().try_insert(account),
            None => false,
        }
    }

    /// Flush all buffered accounts for a specific slot, sending them downstream.
    /// Must be called on the geyser callback thread before sending BlockMeta or
    /// Processed slot status to guarantee accounts arrive first.
    fn flush_account_buffer_for_slot(&self, slot: u64) {
        let entries = match &self.account_buffer {
            Some(buf) => buf.lock().unwrap().drain_slot(slot),
            None => return,
        };
        for account in entries {
            self.send_message(Message::Account(account));
        }
    }

    /// Remove buffered entries for slots <= rooted_slot to prevent memory leaks.
    fn cleanup_account_buffer(&self, rooted_slot: u64) {
        if let Some(buf) = &self.account_buffer {
            buf.lock().unwrap().cleanup(rooted_slot);
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

        // Extract account buffer config before moving config into async block
        let account_buffer_config = config.account_buffer.clone();

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

        let (snapshot_channel, grpc_channel, grpc_shutdown, prometheus, raw_client_channels) =
            runtime.block_on(async move {
                if let Some(config) = config.clickhouse {
                    clickhouse_sink::init(config)
                        .await
                        .expect("Failed to setup clickhouse");
                }

                let (debug_client_tx, debug_client_rx) = mpsc::unbounded_channel();

                // Create shared raw client channels
                let raw_client_channels = Arc::new(RwLock::new(Vec::new()));

                let (snapshot_channel, grpc_channel, grpc_shutdown) = GrpcService::create(
                    config.tokio,
                    config.grpc,
                    config.debug_clients_http.then_some(debug_client_tx),
                    raw_client_channels.clone(),
                    is_reload,
                )
                .await
                .map_err(|error| GeyserPluginError::Custom(format!("{error:?}").into()))?;
                let prometheus = PrometheusService::new(
                    config.prometheus,
                    config.debug_clients_http.then_some(debug_client_rx),
                )
                .await
                .map_err(|error| GeyserPluginError::Custom(Box::new(error)))?;

                #[cfg(feature = "statsd")]
                {
                    let recorder = StatsdBuilder::from("0.0.0.0", 7998)
                        .with_queue_size(50_000)
                        .with_buffer_size(1024)
                        .build(Some("yellowstone_geyser"))
                        .expect("Could not create StatsdRecorder");

                    set_global_recorder(recorder).expect("Could not set global recorder");
                }

                Ok::<_, GeyserPluginError>((
                    snapshot_channel,
                    grpc_channel,
                    grpc_shutdown,
                    prometheus,
                    raw_client_channels,
                ))
            })?;

        let account_buffer = account_buffer_config.map(|cfg| {
            log::info!(
                "Account buffer enabled: size_threshold={}",
                cfg.size_threshold
            );
            Mutex::new(AccountBufferState::new(cfg.size_threshold))
        });

        self.inner = Some(PluginInner {
            runtime,
            snapshot_channel: Mutex::new(snapshot_channel),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel,
            grpc_shutdown,
            prometheus,
            raw_client_channels,
            account_buffer,
        });

        Ok(())
    }

    fn on_unload(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.grpc_shutdown.notify_one();
            drop(inner.grpc_channel);
            inner.prometheus.shutdown();
            inner.runtime.shutdown_timeout(Duration::from_secs(30));
        }
    }

    fn update_account(
        &self,
        account: ReplicaAccountInfoVersions,
        slot: u64,
        is_startup: bool,
    ) -> PluginResult<()> {
        self.with_inner(|inner| {
            let account = match account {
                ReplicaAccountInfoVersions::V0_0_1(_info) => {
                    unreachable!("ReplicaAccountInfoVersions::V0_0_1 is not supported")
                }
                ReplicaAccountInfoVersions::V0_0_2(_info) => {
                    unreachable!("ReplicaAccountInfoVersions::V0_0_2 is not supported")
                }
                ReplicaAccountInfoVersions::V0_0_3(info) => info,
            };

            if is_startup {
                if let Some(channel) = inner.snapshot_channel.lock().unwrap().as_ref() {
                    let message =
                        Message::Account(MessageAccount::from_geyser(account, slot, is_startup));

                    clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));

                    match channel.send(Box::new(message)) {
                        Ok(()) => metrics::message_queue_size_inc(),
                        Err(_) => {
                            if !inner.snapshot_channel_closed.swap(true, Ordering::Relaxed) {
                                log::error!(
                                    "failed to send message to startup queue: channel closed"
                                )
                            }
                        }
                    }
                }
            } else {
                let message_account = MessageAccount::from_geyser(account, slot, is_startup);
                let message = Message::Account(message_account.clone());
                clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));

                if !inner.try_buffer_account(message_account) {
                    inner.send_message(message);
                }
            }

            Ok(())
        })
    }

    fn notify_end_of_startup(&self) -> PluginResult<()> {
        self.with_inner(|inner| {
            let _snapshot_channel = inner.snapshot_channel.lock().unwrap().take();
            Ok(())
        })
    }

    fn update_slot_status(
        &self,
        slot: u64,
        parent: Option<u64>,
        status: &SlotStatus,
    ) -> PluginResult<()> {
        self.with_inner(|inner| {
            // Flush buffered accounts before sending Processed slot notification
            if matches!(status, SlotStatus::Processed) {
                inner.flush_account_buffer_for_slot(slot);
            }

            let message = Message::Slot(MessageSlot::from_geyser(slot, parent, status));
            clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));
            inner.send_message(message);
            metrics::update_slot_status(status, slot);

            // Clean up stale buffer entries on Rooted
            if matches!(status, SlotStatus::Rooted) {
                inner.cleanup_account_buffer(slot);
            }

            Ok(())
        })
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
            clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));
            inner.send_message(message);

            Ok(())
        })
    }

    fn notify_entry(&self, entry: ReplicaEntryInfoVersions) -> PluginResult<()> {
        self.with_inner(|inner| {
            #[allow(clippy::infallible_destructuring_match)]
            let entry = match entry {
                ReplicaEntryInfoVersions::V0_0_1(_entry) => {
                    unreachable!("ReplicaEntryInfoVersions::V0_0_1 is not supported")
                }
                ReplicaEntryInfoVersions::V0_0_2(entry) => entry,
            };

            let message = Message::Entry(Arc::new(MessageEntry::from_geyser(entry)));
            clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));
            inner.send_message(message);

            Ok(())
        })
    }

    fn notify_block_metadata(&self, blockinfo: ReplicaBlockInfoVersions<'_>) -> PluginResult<()> {
        self.with_inner(|inner| {
            let blockinfo = match blockinfo {
                ReplicaBlockInfoVersions::V0_0_1(_info) => {
                    unreachable!("ReplicaBlockInfoVersions::V0_0_1 is not supported")
                }
                ReplicaBlockInfoVersions::V0_0_2(_info) => {
                    unreachable!("ReplicaBlockInfoVersions::V0_0_2 is not supported")
                }
                ReplicaBlockInfoVersions::V0_0_3(_info) => {
                    unreachable!("ReplicaBlockInfoVersions::V0_0_3 is not supported")
                }
                ReplicaBlockInfoVersions::V0_0_4(info) => info,
            };

            // Flush buffered accounts for this slot before block_meta (critical for block reconstruction)
            inner.flush_account_buffer_for_slot(blockinfo.slot);

            let message = Message::BlockMeta(Arc::new(MessageBlockMeta::from_geyser(blockinfo)));
            clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));
            inner.send_message(message);

            Ok(())
        })
    }

    fn account_data_notifications_enabled(&self) -> bool {
        true
    }

    fn account_data_snapshot_notifications_enabled(&self) -> bool {
        false
    }

    fn transaction_notifications_enabled(&self) -> bool {
        true
    }

    fn entry_notifications_enabled(&self) -> bool {
        true
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

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::Timestamp;
    use yellowstone_grpc_proto::plugin::message::MessageAccountInfo;

    fn make_test_inner(
        threshold: Option<usize>,
    ) -> (PluginInner, mpsc::UnboundedReceiver<Message>) {
        let (grpc_tx, grpc_rx) = mpsc::unbounded_channel();
        let inner = PluginInner {
            runtime: Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            snapshot_channel: Mutex::new(None),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel: grpc_tx,
            grpc_shutdown: Arc::new(Notify::new()),
            prometheus: PrometheusService::new_noop(),
            raw_client_channels: Arc::new(RwLock::new(Vec::new())),
            account_buffer: threshold.map(|t| Mutex::new(AccountBufferState::new(t))),
        };
        (inner, grpc_rx)
    }

    fn make_account(pubkey: Pubkey, slot: u64, data_len: usize, write_version: u64) -> MessageAccount {
        MessageAccount {
            account: Arc::new(MessageAccountInfo {
                pubkey,
                lamports: 1,
                owner: Pubkey::default(),
                executable: false,
                rent_epoch: 0,
                data: vec![0u8; data_len],
                write_version,
                txn_signature: None,
            }),
            slot,
            is_startup: false,
            created_at: Timestamp::default(),
        }
    }

    #[test]
    fn test_small_account_passes_through() {
        let (inner, mut rx) = make_test_inner(Some(1000));
        let account = make_account(Pubkey::new_unique(), 1, 500, 1);
        assert!(!inner.try_buffer_account(account));
        let buf = inner.account_buffer.as_ref().unwrap().lock().unwrap();
        assert!(buf.slots.is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_large_account_is_buffered() {
        let (inner, _rx) = make_test_inner(Some(1000));
        let account = make_account(Pubkey::new_unique(), 1, 2000, 1);
        assert!(inner.try_buffer_account(account));
        let buf = inner.account_buffer.as_ref().unwrap().lock().unwrap();
        assert_eq!(buf.slots.len(), 1);
        assert_eq!(buf.slots[&1].len(), 1);
    }

    #[test]
    fn test_dedup_keeps_highest_write_version() {
        let (inner, _rx) = make_test_inner(Some(1000));
        let pk = Pubkey::new_unique();

        inner.try_buffer_account(make_account(pk, 1, 2000, 1));
        inner.try_buffer_account(make_account(pk, 1, 2000, 3));
        inner.try_buffer_account(make_account(pk, 1, 2000, 2)); // lower, should be ignored

        let buf = inner.account_buffer.as_ref().unwrap().lock().unwrap();
        assert_eq!(buf.slots[&1].len(), 1);
        assert_eq!(buf.slots[&1][&pk].account.write_version, 3);
    }

    #[test]
    fn test_flush_for_slot_sends_and_removes() {
        let (inner, mut rx) = make_test_inner(Some(1000));
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();

        inner.try_buffer_account(make_account(pk1, 1, 2000, 1));
        inner.try_buffer_account(make_account(pk2, 2, 2000, 1));

        inner.flush_account_buffer_for_slot(1);

        // Only slot 2 should remain
        let buf = inner.account_buffer.as_ref().unwrap().lock().unwrap();
        assert_eq!(buf.slots.len(), 1);
        assert!(buf.slots.contains_key(&2));
        drop(buf);

        // One message should have been sent
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::Account(_)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_cleanup_removes_old_slots() {
        let (inner, _rx) = make_test_inner(Some(1000));

        inner.try_buffer_account(make_account(Pubkey::new_unique(), 5, 2000, 1));
        inner.try_buffer_account(make_account(Pubkey::new_unique(), 10, 2000, 1));
        inner.try_buffer_account(make_account(Pubkey::new_unique(), 15, 2000, 1));

        inner.cleanup_account_buffer(10);

        let buf = inner.account_buffer.as_ref().unwrap().lock().unwrap();
        assert_eq!(buf.slots.len(), 1);
        assert!(buf.slots.contains_key(&15));
    }

    #[test]
    fn test_no_buffering_when_disabled() {
        let (inner, _rx) = make_test_inner(None);
        let account = make_account(Pubkey::new_unique(), 1, 999999, 1);
        assert!(!inner.try_buffer_account(account));
        assert!(inner.account_buffer.is_none());
    }
}
