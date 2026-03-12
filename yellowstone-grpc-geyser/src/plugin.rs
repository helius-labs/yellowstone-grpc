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
        SlotStatus as MessageSlotStatus,
    },
};

#[cfg(feature = "statsd")]
use ::metrics::set_global_recorder;
#[cfg(feature = "statsd")]
use metrics_exporter_statsd::StatsdBuilder;

/// Single-threaded account buffer owned by the buffer consumer task.
/// No locking needed — only accessed from one async task.
struct AccountBuffer {
    /// slot -> (pubkey -> account). Nested map gives O(1) slot lookup.
    slots: HashMap<u64, HashMap<Pubkey, MessageAccount>>,
}

impl AccountBuffer {
    fn new() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }

    /// Insert or update, keeping the highest write_version.
    fn upsert(&mut self, account: MessageAccount) {
        let slot_map = self.slots.entry(account.slot).or_default();
        match slot_map.get(&account.account.pubkey) {
            Some(existing) if existing.account.write_version >= account.account.write_version => {}
            _ => {
                slot_map.insert(account.account.pubkey, account);
            }
        }
    }

    /// Remove and return a buffered entry for (slot, pubkey), if present.
    fn remove_entry(&mut self, slot: u64, pubkey: &Pubkey) -> Option<MessageAccount> {
        let slot_map = self.slots.get_mut(&slot)?;
        let entry = slot_map.remove(pubkey);
        if slot_map.is_empty() {
            self.slots.remove(&slot);
        }
        entry
    }

    /// Drain all entries for a specific slot.
    fn drain_slot(&mut self, slot: u64) -> Vec<MessageAccount> {
        self.slots
            .remove(&slot)
            .map(|m| m.into_values().collect())
            .unwrap_or_default()
    }

    /// Drain all entries across all slots.
    fn drain_all(&mut self) -> Vec<MessageAccount> {
        let mut out = Vec::new();
        for (_slot, slot_map) in self.slots.drain() {
            out.extend(slot_map.into_values());
        }
        out
    }

}

/// Spawns the buffer consumer task. Receives all messages, deduplicates large
/// accounts, and forwards to both raw clients and the grpc pipeline. Runs on
/// a single thread with no locking — the channel serializes all access.
fn spawn_buffer_task(
    runtime: &Runtime,
    mut rx: mpsc::UnboundedReceiver<Message>,
    grpc_tx: mpsc::UnboundedSender<Message>,
    raw_client_channels: Arc<RwLock<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
    size_threshold: usize,
    flush_interval: Duration,
    shutdown: Arc<Notify>,
) {
    runtime.spawn(async move {
        let mut buffer = AccountBuffer::new();
        let mut flush_timer = tokio::time::interval(flush_interval);
        // The first tick completes immediately; skip it so we don't flush an empty buffer.
        flush_timer.tick().await;

        let forward = |msg: Message| {
            // Send to raw clients (deduplicated, same as grpc clients)
            if let Ok(raw_clients) = raw_client_channels.read() {
                for (id, tx) in raw_clients.iter() {
                    if tx.send(msg.clone()).is_err() {
                        log::warn!("Raw client {} channel disconnected", id);
                    }
                }
            }

            if grpc_tx.send(msg).is_ok() {
                metrics::message_queue_size_inc();
            }
        };

        loop {
            tokio::select! {
                biased;
                msg = rx.recv() => {
                    let msg = match msg {
                        Some(m) => m,
                        None => break,
                    };

                    match msg {
                        Message::Account(account) if !account.is_startup => {
                            if account.account.data.len() >= size_threshold {
                                // Large account: buffer/dedup
                                buffer.upsert(account);
                            } else {
                                // Small account: evict stale buffered entry if present
                                buffer.remove_entry(account.slot, &account.account.pubkey);
                                forward(Message::Account(account));
                            }
                        }
                        Message::BlockMeta(ref meta) => {
                            // Flush buffer for this slot before forwarding block_meta
                            let slot = meta.slot();
                            for account in buffer.drain_slot(slot) {
                                forward(Message::Account(account));
                            }
                            forward(msg);
                        }
                        _ => {
                            // Everything else: forward immediately
                            forward(msg);
                        }
                    }
                }
                _ = flush_timer.tick() => {
                    // Periodically flush all deduped accounts
                    for account in buffer.drain_all() {
                        forward(Message::Account(account));
                    }
                }
                _ = shutdown.notified() => break,
            }
        }
    });
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
    /// When account buffering is enabled, messages go through this channel
    /// to a single-threaded consumer that deduplicates large accounts.
    /// When None, messages go directly to grpc_channel.
    buffer_tx: Option<mpsc::UnboundedSender<Message>>,
}

impl PluginInner {
    fn send_message(&self, message: Message) {
        match &self.buffer_tx {
            Some(tx) => {
                // Buffer task handles forwarding to both raw clients and grpc
                let _ = tx.send(message);
            }
            None => {
                // No buffering: send directly to raw clients and grpc
                if let Ok(raw_clients) = self.raw_client_channels.read() {
                    for (id, tx) in raw_clients.iter() {
                        if tx.send(message.clone()).is_err() {
                            log::warn!("Raw client {} channel disconnected", id);
                        }
                    }
                }

                if self.grpc_channel.send(message).is_ok() {
                    metrics::message_queue_size_inc();
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

        // Spawn buffer task if account buffering is configured
        let buffer_tx = account_buffer_config.map(|cfg| {
            let flush_interval = Duration::from_millis(cfg.flush_interval_ms);
            log::info!(
                "Account buffer enabled: size_threshold={}, flush_interval={:?}",
                cfg.size_threshold,
                flush_interval,
            );
            let (tx, rx) = mpsc::unbounded_channel();
            spawn_buffer_task(
                &runtime,
                rx,
                grpc_channel.clone(),
                raw_client_channels.clone(),
                cfg.size_threshold,
                flush_interval,
                grpc_shutdown.clone(),
            );
            tx
        });

        self.inner = Some(PluginInner {
            runtime,
            snapshot_channel: Mutex::new(snapshot_channel),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel,
            grpc_shutdown,
            prometheus,
            raw_client_channels,
            buffer_tx,
        });

        Ok(())
    }

    fn on_unload(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner.grpc_shutdown.notify_one();
            drop(inner.buffer_tx);
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
                let message =
                    Message::Account(MessageAccount::from_geyser(account, slot, is_startup));
                clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));
                inner.send_message(message);
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
            let message = Message::Slot(MessageSlot::from_geyser(slot, parent, status));
            clickhouse_sink::event::record(message.get_latency_payload("ys_geyser_recv"));
            inner.send_message(message);
            metrics::update_slot_status(status, slot);
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
    use yellowstone_grpc_proto::{
        geyser::SubscribeUpdateBlockMeta,
        plugin::message::MessageAccountInfo,
    };

    /// Creates a test setup with a buffer consumer task.
    /// Returns the PluginInner (which sends to the buffer task) and the
    /// grpc_rx (which receives the final output after buffering).
    fn make_test_inner(
        threshold: Option<usize>,
    ) -> (PluginInner, mpsc::UnboundedReceiver<Message>) {
        let (grpc_tx, grpc_rx) = mpsc::unbounded_channel();
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let raw_client_channels = Arc::new(RwLock::new(Vec::new()));
        let buffer_tx = threshold.map(|t| {
            let (tx, rx) = mpsc::unbounded_channel();
            spawn_buffer_task(
                &runtime,
                rx,
                grpc_tx.clone(),
                raw_client_channels.clone(),
                t,
                Duration::from_secs(3600), // long interval so tests control flushing explicitly
                Arc::new(Notify::new()),
            );
            tx
        });

        let inner = PluginInner {
            runtime,
            snapshot_channel: Mutex::new(None),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel: grpc_tx,
            grpc_shutdown: Arc::new(Notify::new()),
            prometheus: PrometheusService::new_noop(),
            raw_client_channels: Arc::new(RwLock::new(Vec::new())),
            buffer_tx,
        };
        (inner, grpc_rx)
    }

    fn make_account(
        pubkey: Pubkey,
        slot: u64,
        data_len: usize,
        write_version: u64,
    ) -> MessageAccount {
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

    fn make_block_meta(slot: u64) -> Message {
        Message::BlockMeta(Arc::new(MessageBlockMeta::from_update_oneof(
            SubscribeUpdateBlockMeta {
                slot,
                ..Default::default()
            },
            Timestamp::default(),
        )))
    }

    /// Let the tokio runtime process pending tasks (the buffer consumer).
    fn flush_runtime(inner: &PluginInner) {
        inner.runtime.block_on(async {
            // Sleep briefly to let the buffer task fully process pending messages.
            tokio::time::sleep(Duration::from_millis(10)).await;
        });
    }

    #[test]
    fn test_small_account_passes_through() {
        let (inner, mut rx) = make_test_inner(Some(1000));
        let account = make_account(Pubkey::new_unique(), 1, 500, 1);
        inner.send_message(Message::Account(account));
        flush_runtime(&inner);

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::Account(_)));
    }

    #[test]
    fn test_large_account_is_buffered() {
        let (inner, mut rx) = make_test_inner(Some(1000));
        let account = make_account(Pubkey::new_unique(), 1, 2000, 1);
        inner.send_message(Message::Account(account));
        flush_runtime(&inner);

        // Large account should be buffered, not forwarded yet
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_dedup_keeps_highest_write_version() {
        let (inner, mut rx) = make_test_inner(Some(1000));
        let pk = Pubkey::new_unique();

        inner.send_message(Message::Account(make_account(pk, 1, 2000, 1)));
        inner.send_message(Message::Account(make_account(pk, 1, 2000, 3)));
        inner.send_message(Message::Account(make_account(pk, 1, 2000, 2)));

        // Flush via BlockMeta
        let block_meta = make_block_meta(1);
        inner.send_message(block_meta);
        flush_runtime(&inner);

        // Should get one account (highest write_version) then block_meta
        let msg = rx.try_recv().unwrap();
        if let Message::Account(account) = msg {
            assert_eq!(account.account.write_version, 3);
        } else {
            panic!("expected Account message");
        }
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::BlockMeta(_)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_block_meta_flushes_slot() {
        let (inner, mut rx) = make_test_inner(Some(1000));
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();

        // Buffer accounts for two different slots
        inner.send_message(Message::Account(make_account(pk1, 1, 2000, 1)));
        inner.send_message(Message::Account(make_account(pk2, 2, 2000, 1)));

        // BlockMeta for slot 1 should only flush slot 1
        let block_meta = make_block_meta(1);
        inner.send_message(block_meta);
        flush_runtime(&inner);

        // Should get: Account(slot=1), BlockMeta(slot=1)
        let msg = rx.try_recv().unwrap();
        if let Message::Account(account) = msg {
            assert_eq!(account.slot, 1);
        } else {
            panic!("expected Account message");
        }
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::BlockMeta(_)));
        // Slot 2 still buffered
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_shrinking_account_evicts_stale_entry() {
        let (inner, mut rx) = make_test_inner(Some(1000));
        let pk = Pubkey::new_unique();

        // Large account gets buffered
        inner.send_message(Message::Account(make_account(pk, 1, 2000, 1)));
        // Same account shrinks below threshold — should evict and forward
        inner.send_message(Message::Account(make_account(pk, 1, 500, 2)));
        flush_runtime(&inner);

        // Small account should have been forwarded
        let msg = rx.try_recv().unwrap();
        if let Message::Account(account) = msg {
            assert_eq!(account.account.data.len(), 500);
            assert_eq!(account.account.write_version, 2);
        } else {
            panic!("expected Account message");
        }

        // Flush via BlockMeta — nothing should come out (stale entry was evicted)
        let block_meta = make_block_meta(1);
        inner.send_message(block_meta);
        flush_runtime(&inner);

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::BlockMeta(_)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_no_buffering_when_disabled() {
        let (inner, mut rx) = make_test_inner(None);
        let account = make_account(Pubkey::new_unique(), 1, 999999, 1);
        inner.send_message(Message::Account(account));

        // Without buffering, goes straight to grpc_channel
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::Account(_)));
    }

    #[test]
    fn test_non_account_messages_pass_through() {
        let (inner, mut rx) = make_test_inner(Some(1000));

        let slot_msg = Message::Slot(MessageSlot {
            slot: 1,
            parent: Some(0),
            status: MessageSlotStatus::Processed,
            dead_error: None,
            created_at: Timestamp::default(),
        });
        inner.send_message(slot_msg);
        flush_runtime(&inner);

        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::Slot(_)));
    }
}
