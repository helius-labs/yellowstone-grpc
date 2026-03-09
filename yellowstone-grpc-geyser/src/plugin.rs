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

#[derive(Debug)]
pub struct PluginInner {
    runtime: Runtime,
    snapshot_channel: Mutex<Option<crossbeam_channel::Sender<Box<Message>>>>,
    snapshot_channel_closed: AtomicBool,
    grpc_channel: mpsc::UnboundedSender<Message>,
    grpc_shutdown: Arc<Notify>,
    prometheus: PrometheusService,
    raw_client_channels: Arc<RwLock<Vec<(u64, crossbeam_channel::Sender<Message>)>>>,
    /// Buffer for BPF Upgradeable Loader account updates during program deployments.
    /// Keyed by (slot, pubkey), only the highest write_version is kept per key.
    /// Flushed when the slot reaches Processed status.
    deploy_buffer: Mutex<HashMap<(u64, Pubkey), MessageAccount>>,
}

impl PluginInner {
    fn send_message(&self, message: Message) {
        // Send to raw clients first (bypasses all processing)
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

        // Then send to regular geyser_loop pipeline
        if self.grpc_channel.send(message).is_ok() {
            metrics::message_queue_size_inc();
        }
    }

    /// BPFLoaderUpgradeab1e11111111111111111111111
    const BPF_LOADER_UPGRADEABLE_ID: Pubkey =
        solana_sdk::pubkey!("BPFLoaderUpgradeab1e11111111111111111111111");

    /// Returns true if this account is owned by the BPF Upgradeable Loader.
    fn is_bpf_loader_account(owner: &Pubkey) -> bool {
        *owner == Self::BPF_LOADER_UPGRADEABLE_ID
    }

    /// Returns true if this is a non-executable BPF loader account (programdata
    /// being written during a multi-step upload). Executable BPF accounts are
    /// already-deployed programs whose modifications should not be buffered.
    fn should_buffer_account(owner: &Pubkey, executable: bool) -> bool {
        Self::is_bpf_loader_account(owner) && !executable
    }

    /// Remove a buffered entry for (slot, pubkey), if present. Called when an
    /// executable version of the account arrives (finalization), so we don't
    /// later flush stale non-executable data.
    fn evict_from_deploy_buffer(&self, slot: u64, pubkey: &Pubkey) {
        let mut buffer = self.deploy_buffer.lock().unwrap();
        if buffer.remove(&(slot, *pubkey)).is_some() {
            log::info!(
                "Evicted buffered programdata for {} in slot {} (account finalized)",
                pubkey,
                slot
            );
        }
    }

    /// Buffer a BPF loader account update, keeping only the highest write_version per (slot, pubkey).
    fn buffer_deploy_account(&self, account: MessageAccount) {
        let key = (account.slot, *account.pubkey());
        let mut buffer = self.deploy_buffer.lock().unwrap();
        match buffer.entry(key) {
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                if account.write_version() > entry.get().write_version() {
                    entry.insert(account);
                }
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(account);
            }
        }
    }

    /// Flush all buffered deploy accounts for the given slot, sending them downstream.
    fn flush_deploy_buffer(&self, slot: u64) {
        let accounts: Vec<MessageAccount> = {
            let mut buffer = self.deploy_buffer.lock().unwrap();
            let keys_to_flush: Vec<(u64, Pubkey)> = buffer
                .keys()
                .filter(|(s, _)| *s == slot)
                .cloned()
                .collect();
            keys_to_flush
                .into_iter()
                .filter_map(|key| buffer.remove(&key))
                .collect()
        };

        if !accounts.is_empty() {
            log::info!(
                "Flushing {} buffered BPF loader account update(s) for slot {}",
                accounts.len(),
                slot
            );
            for account in accounts {
                self.send_message(Message::Account(account));
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

        self.inner = Some(PluginInner {
            runtime,
            snapshot_channel: Mutex::new(snapshot_channel),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel,
            grpc_shutdown,
            prometheus,
            raw_client_channels,
            deploy_buffer: Mutex::new(HashMap::new()),
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
                let msg_account = MessageAccount::from_geyser(account, slot, is_startup);

                if PluginInner::should_buffer_account(&msg_account.account.owner, msg_account.account.executable) {
                    // Non-executable BPF loader account (programdata upload in progress).
                    // Buffer and dedupe by write_version until slot is processed.
                    clickhouse_sink::event::record(
                        Message::Account(msg_account.clone())
                            .get_latency_payload("ys_geyser_recv"),
                    );
                    inner.buffer_deploy_account(msg_account);
                } else {
                    // If this is an executable BPF account (finalization), evict any
                    // buffered non-executable entries for this (slot, pubkey) so we
                    // don't flush stale data after the finalized version.
                    if PluginInner::is_bpf_loader_account(&msg_account.account.owner) {
                        inner.evict_from_deploy_buffer(msg_account.slot, msg_account.pubkey());
                    }

                    let message = Message::Account(msg_account);
                    clickhouse_sink::event::record(
                        message.get_latency_payload("ys_geyser_recv"),
                    );
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
            // Flush buffered BPF loader account updates when slot reaches Processed
            if matches!(status, SlotStatus::Processed) {
                inner.flush_deploy_buffer(slot);
            }

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
    use solana_sdk::pubkey::Pubkey;
    use std::time::SystemTime;
    use yellowstone_grpc_proto::plugin::message::MessageAccountInfo;

    fn make_account(pubkey: Pubkey, owner: Pubkey, slot: u64, write_version: u64) -> MessageAccount {
        MessageAccount {
            account: Arc::new(MessageAccountInfo {
                pubkey,
                lamports: 1_000_000,
                owner,
                executable: false,
                rent_epoch: 0,
                data: vec![0u8; 64],
                write_version,
                txn_signature: None,
            }),
            slot,
            is_startup: false,
            created_at: Timestamp::from(SystemTime::now()),
        }
    }

    fn make_test_inner() -> PluginInner {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let (grpc_tx, _grpc_rx) = mpsc::unbounded_channel();
        let (snapshot_tx, _snapshot_rx) = crossbeam_channel::unbounded();
        PluginInner {
            runtime,
            snapshot_channel: Mutex::new(Some(snapshot_tx)),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel: grpc_tx,
            grpc_shutdown: Arc::new(Notify::new()),
            prometheus: PrometheusService::new_noop(),
            raw_client_channels: Arc::new(RwLock::new(Vec::new())),
            deploy_buffer: Mutex::new(HashMap::new()),
        }
    }

    #[test]
    fn test_should_buffer_account() {
        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;

        // Non-executable BPF account (upload in progress) => buffer
        assert!(PluginInner::should_buffer_account(&bpf_id, false));

        // Executable BPF account (deployed program) => don't buffer
        assert!(!PluginInner::should_buffer_account(&bpf_id, true));

        // Non-BPF accounts => never buffer
        let random_owner = Pubkey::new_unique();
        assert!(!PluginInner::should_buffer_account(&random_owner, false));
        assert!(!PluginInner::should_buffer_account(&random_owner, true));
    }

    #[test]
    fn test_evict_on_finalization() {
        let inner = make_test_inner();
        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey = Pubkey::new_unique();
        let slot = 100;

        // Simulate upload writes buffered
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, slot, 1));
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, slot, 3));
        assert!(inner.deploy_buffer.lock().unwrap().contains_key(&(slot, pubkey)));

        // Finalization arrives (executable=true) => evict buffer entry
        inner.evict_from_deploy_buffer(slot, &pubkey);
        assert!(!inner.deploy_buffer.lock().unwrap().contains_key(&(slot, pubkey)));

        // Flush should produce nothing for this pubkey
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let (grpc_tx, mut grpc_rx) = mpsc::unbounded_channel();
        // Swap in a working channel to test flush
        let inner2 = PluginInner {
            runtime,
            snapshot_channel: Mutex::new(None),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel: grpc_tx,
            grpc_shutdown: Arc::new(Notify::new()),
            prometheus: PrometheusService::new_noop(),
            raw_client_channels: Arc::new(RwLock::new(Vec::new())),
            deploy_buffer: Mutex::new(HashMap::new()),
        };
        inner2.flush_deploy_buffer(slot);
        assert!(grpc_rx.try_recv().is_err());
    }

    #[test]
    fn test_evict_does_not_affect_other_slots() {
        let inner = make_test_inner();
        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey = Pubkey::new_unique();

        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 100, 1));
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 101, 2));

        // Evict only slot 100
        inner.evict_from_deploy_buffer(100, &pubkey);

        let buffer = inner.deploy_buffer.lock().unwrap();
        assert!(!buffer.contains_key(&(100, pubkey)));
        assert!(buffer.contains_key(&(101, pubkey)));
    }

    #[test]
    fn test_buffer_keeps_highest_write_version() {
        let inner = make_test_inner();
        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey = Pubkey::new_unique();
        let slot = 100;

        // Insert write_version 1
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, slot, 1));
        // Insert write_version 5 (higher)
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, slot, 5));
        // Insert write_version 3 (lower, should be ignored)
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, slot, 3));

        let buffer = inner.deploy_buffer.lock().unwrap();
        let entry = buffer.get(&(slot, pubkey)).unwrap();
        assert_eq!(entry.write_version(), 5);
    }

    #[test]
    fn test_buffer_separates_by_pubkey() {
        let inner = make_test_inner();
        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey_a = Pubkey::new_unique();
        let pubkey_b = Pubkey::new_unique();
        let slot = 100;

        inner.buffer_deploy_account(make_account(pubkey_a, bpf_id, slot, 10));
        inner.buffer_deploy_account(make_account(pubkey_b, bpf_id, slot, 20));

        let buffer = inner.deploy_buffer.lock().unwrap();
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.get(&(slot, pubkey_a)).unwrap().write_version(), 10);
        assert_eq!(buffer.get(&(slot, pubkey_b)).unwrap().write_version(), 20);
    }

    #[test]
    fn test_buffer_separates_by_slot() {
        let inner = make_test_inner();
        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey = Pubkey::new_unique();

        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 100, 1));
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 101, 2));

        let buffer = inner.deploy_buffer.lock().unwrap();
        assert_eq!(buffer.len(), 2);
        assert_eq!(buffer.get(&(100, pubkey)).unwrap().write_version(), 1);
        assert_eq!(buffer.get(&(101, pubkey)).unwrap().write_version(), 2);
    }

    #[test]
    fn test_flush_sends_and_removes_buffered_accounts() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let (grpc_tx, mut grpc_rx) = mpsc::unbounded_channel();
        let inner = PluginInner {
            runtime,
            snapshot_channel: Mutex::new(None),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel: grpc_tx,
            grpc_shutdown: Arc::new(Notify::new()),
            prometheus: PrometheusService::new_noop(),
            raw_client_channels: Arc::new(RwLock::new(Vec::new())),
            deploy_buffer: Mutex::new(HashMap::new()),
        };

        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey = Pubkey::new_unique();

        // Buffer 3 updates, only highest write version should survive
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 100, 1));
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 100, 5));
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 100, 3));

        // Also buffer a different slot
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 101, 10));

        // Flush slot 100 only
        inner.flush_deploy_buffer(100);

        // Should have sent exactly 1 message (the deduplicated account)
        let msg = grpc_rx.try_recv().unwrap();
        match msg {
            Message::Account(account) => {
                assert_eq!(account.write_version(), 5);
                assert_eq!(account.slot, 100);
            }
            _ => panic!("expected Account message"),
        }

        // No more messages for slot 100
        assert!(grpc_rx.try_recv().is_err());

        // Slot 101 should still be in the buffer
        let buffer = inner.deploy_buffer.lock().unwrap();
        assert_eq!(buffer.len(), 1);
        assert_eq!(buffer.get(&(101, pubkey)).unwrap().write_version(), 10);
    }

    #[test]
    fn test_flush_empty_buffer_is_noop() {
        let inner = make_test_inner();
        // Flushing an empty buffer should not panic or send any messages
        inner.flush_deploy_buffer(100);
        let buffer = inner.deploy_buffer.lock().unwrap();
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_evict_nonexistent_key_is_noop() {
        let inner = make_test_inner();
        let pubkey = Pubkey::new_unique();
        // Should not panic when evicting a key that was never buffered
        inner.evict_from_deploy_buffer(100, &pubkey);
        assert!(inner.deploy_buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn test_flush_multiple_pubkeys_in_same_slot() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let (grpc_tx, mut grpc_rx) = mpsc::unbounded_channel();
        let inner = PluginInner {
            runtime,
            snapshot_channel: Mutex::new(None),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel: grpc_tx,
            grpc_shutdown: Arc::new(Notify::new()),
            prometheus: PrometheusService::new_noop(),
            raw_client_channels: Arc::new(RwLock::new(Vec::new())),
            deploy_buffer: Mutex::new(HashMap::new()),
        };

        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey_a = Pubkey::new_unique();
        let pubkey_b = Pubkey::new_unique();
        let slot = 200;

        // Multiple writes per pubkey, different pubkeys in the same slot
        inner.buffer_deploy_account(make_account(pubkey_a, bpf_id, slot, 1));
        inner.buffer_deploy_account(make_account(pubkey_a, bpf_id, slot, 4));
        inner.buffer_deploy_account(make_account(pubkey_b, bpf_id, slot, 2));
        inner.buffer_deploy_account(make_account(pubkey_b, bpf_id, slot, 7));

        inner.flush_deploy_buffer(slot);

        // Should receive exactly 2 messages (one per pubkey, each with highest write_version)
        let mut received = Vec::new();
        while let Ok(msg) = grpc_rx.try_recv() {
            match msg {
                Message::Account(account) => received.push(account),
                _ => panic!("expected Account message"),
            }
        }
        assert_eq!(received.len(), 2);

        received.sort_by_key(|a| *a.pubkey());
        let mut expected = vec![pubkey_a, pubkey_b];
        expected.sort();

        for (account, expected_pubkey) in received.iter().zip(expected.iter()) {
            assert_eq!(account.pubkey(), expected_pubkey);
            assert_eq!(account.slot, slot);
        }

        // Verify highest write versions were kept
        let a_account = received.iter().find(|a| a.pubkey() == &pubkey_a).unwrap();
        let b_account = received.iter().find(|a| a.pubkey() == &pubkey_b).unwrap();
        assert_eq!(a_account.write_version(), 4);
        assert_eq!(b_account.write_version(), 7);

        // Buffer should be empty after flush
        assert!(inner.deploy_buffer.lock().unwrap().is_empty());
    }

    #[test]
    fn test_flush_sends_to_raw_clients() {
        let runtime = Builder::new_current_thread().enable_all().build().unwrap();
        let (grpc_tx, _grpc_rx) = mpsc::unbounded_channel();
        let (raw_tx, raw_rx) = crossbeam_channel::unbounded();
        let raw_clients = Arc::new(RwLock::new(vec![(1u64, raw_tx)]));
        let inner = PluginInner {
            runtime,
            snapshot_channel: Mutex::new(None),
            snapshot_channel_closed: AtomicBool::new(false),
            grpc_channel: grpc_tx,
            grpc_shutdown: Arc::new(Notify::new()),
            prometheus: PrometheusService::new_noop(),
            raw_client_channels: raw_clients,
            deploy_buffer: Mutex::new(HashMap::new()),
        };

        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey = Pubkey::new_unique();
        inner.buffer_deploy_account(make_account(pubkey, bpf_id, 100, 5));

        inner.flush_deploy_buffer(100);

        // Raw client should also receive the flushed message
        let msg = raw_rx.try_recv().unwrap();
        match msg {
            Message::Account(account) => {
                assert_eq!(account.pubkey(), &pubkey);
                assert_eq!(account.write_version(), 5);
            }
            _ => panic!("expected Account message"),
        }
    }

    #[test]
    fn test_buffer_preserves_account_data() {
        let inner = make_test_inner();
        let bpf_id = PluginInner::BPF_LOADER_UPGRADEABLE_ID;
        let pubkey = Pubkey::new_unique();
        let slot = 100;

        let mut account = make_account(pubkey, bpf_id, slot, 42);
        Arc::make_mut(&mut account.account).lamports = 999_999;
        Arc::make_mut(&mut account.account).data = vec![0xAB; 128];
        inner.buffer_deploy_account(account);

        let buffer = inner.deploy_buffer.lock().unwrap();
        let entry = buffer.get(&(slot, pubkey)).unwrap();
        assert_eq!(entry.account.lamports, 999_999);
        assert_eq!(entry.account.data, vec![0xAB; 128]);
        assert_eq!(entry.account.owner, bpf_id);
        assert_eq!(entry.account.pubkey, pubkey);
        assert_eq!(entry.slot, slot);
    }
}
