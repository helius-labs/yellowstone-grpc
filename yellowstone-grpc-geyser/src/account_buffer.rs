use {
    crate::metrics,
    solana_sdk::pubkey::Pubkey,
    std::{
        collections::{HashMap, HashSet},
        sync::{Arc, RwLock},
        time::Duration,
    },
    tokio::{
        runtime::Runtime,
        sync::{mpsc, Notify},
    },
    yellowstone_grpc_proto::plugin::message::{
        Message, MessageAccount, SlotStatus as MessageSlotStatus,
    },
};

/// Forwards a message to raw clients and the grpc pipeline.
pub fn forward_message(
    msg: Message,
    grpc_tx: &mpsc::UnboundedSender<Message>,
    raw_client_channels: &RwLock<Vec<(u64, crossbeam_channel::Sender<Message>)>>,
) {
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
}

/// Tracks unique write_versions per (slot, pubkey) so we can normalize them.
///
/// Write versions are not consistent across YS - each Agave node sets it to 0 on startup
/// and increments for each update. Given that account updates are processed in the same order
/// across nodes, we normalize by tracking the count of unique write_versions seen for each
/// (slot, pubkey) and using that count as a deterministic replacement.
pub struct WriteVersionTracker {
    /// slot -> (pubkey -> set of write_versions seen)
    slot_versions: HashMap<u64, HashMap<Pubkey, HashSet<u64>>>,
}

impl WriteVersionTracker {
    pub fn new() -> Self {
        Self {
            slot_versions: HashMap::new(),
        }
    }

    /// Normalize the write_version for an account message.
    /// Returns a new MessageAccount with write_version = slot * 10_000_000 + count_of_unique_versions.
    pub fn normalize(&mut self, account: MessageAccount) -> MessageAccount {
        let pubkey_versions = self
            .slot_versions
            .entry(account.slot)
            .or_default()
            .entry(account.account.pubkey)
            .or_default();

        pubkey_versions.insert(account.account.write_version);

        let normalized = account.slot * 10_000_000 + pubkey_versions.len() as u64;

        let mut info = (*account.account).clone();
        info.write_version = normalized;

        MessageAccount {
            account: Arc::new(info),
            slot: account.slot,
            is_startup: account.is_startup,
            created_at: account.created_at,
        }
    }

    /// Remove tracking data for all slots earlier than the finalized slot.
    fn on_finalized(&mut self, slot: u64) {
        self.slot_versions.retain(|&s, _| s >= slot);
    }
}

/// Single-threaded account buffer owned by the buffer consumer task.
/// No locking needed — only accessed from one async task.
struct AccountBuffer {
    /// slot -> (pubkey -> account). Nested map gives O(1) slot lookup.
    slots: HashMap<u64, HashMap<Pubkey, MessageAccount>>,
}

impl AccountBuffer {
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }

    /// Insert or update, keeping the highest write_version.
    fn upsert(&mut self, account: MessageAccount) {
        let slot_map = self.slots.entry(account.slot).or_default();
        if let Some(existing) = slot_map.get(&account.account.pubkey) {
            if existing.account.write_version >= account.account.write_version {
                return;
            }
        }
        slot_map.insert(account.account.pubkey, account);
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
pub fn spawn_buffer_task(
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
        let mut tracker = WriteVersionTracker::new();
        let mut flush_timer = tokio::time::interval(flush_interval);
        // The first tick completes immediately; skip it so we don't flush an empty buffer.
        flush_timer.tick().await;

        let forward = |msg: Message| {
            forward_message(msg, &grpc_tx, &raw_client_channels);
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
                        Message::Account(account) => {
                            let account = tracker.normalize(account);
                            if account.account.data.len() >= size_threshold {
                                buffer.upsert(account);
                            } else {
                                // Small account: evict stale buffered entry if present
                                buffer.remove_entry(account.slot, &account.account.pubkey);
                                forward(Message::Account(account));
                            }
                        }
                        Message::Slot(ref slot_msg)
                            if slot_msg.status == MessageSlotStatus::Finalized =>
                        {
                            tracker.on_finalized(slot_msg.slot);
                            forward(msg);
                        }
                        Message::Slot(ref slot_msg)
                            if slot_msg.status == MessageSlotStatus::Processed =>
                        {
                            // Flush buffer for this slot before forwarding slot processed
                            for account in buffer.drain_slot(slot_msg.slot) {
                                forward(Message::Account(account));
                            }
                            forward(msg);
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
                            forward(msg);
                        }
                    }
                }
                _ = flush_timer.tick() => {
                    for account in buffer.drain_all() {
                        forward(Message::Account(account));
                    }
                }
                _ = shutdown.notified() => break,
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost_types::Timestamp;
    use tokio::runtime::Builder;
    use yellowstone_grpc_proto::{
        geyser::SubscribeUpdateBlockMeta,
        plugin::message::{
            MessageAccountInfo, MessageBlockMeta, MessageSlot, SlotStatus as MessageSlotStatus,
        },
    };

    struct TestHarness {
        tx: mpsc::UnboundedSender<Message>,
        grpc_rx: mpsc::UnboundedReceiver<Message>,
        runtime: Runtime,
    }

    fn make_test_harness(threshold: usize) -> TestHarness {
        let (grpc_tx, grpc_rx) = mpsc::unbounded_channel();
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let raw_client_channels = Arc::new(RwLock::new(Vec::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_buffer_task(
            &runtime,
            rx,
            grpc_tx,
            raw_client_channels,
            threshold,
            Duration::from_secs(3600), // long interval so tests control flushing explicitly
            Arc::new(Notify::new()),
        );

        TestHarness {
            tx,
            grpc_rx,
            runtime,
        }
    }

    impl TestHarness {
        fn send(&self, msg: Message) {
            self.tx.send(msg).unwrap();
        }

        fn flush(&self) {
            self.runtime.block_on(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
            });
        }

        fn try_recv(&mut self) -> Option<Message> {
            self.grpc_rx.try_recv().ok()
        }
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

    #[test]
    fn test_small_account_passes_through() {
        let mut h = make_test_harness(1000);
        h.send(Message::Account(make_account(Pubkey::new_unique(), 1, 500, 1)));
        h.flush();
        assert!(matches!(h.try_recv(), Some(Message::Account(_))));
    }

    #[test]
    fn test_large_account_is_buffered() {
        let mut h = make_test_harness(1000);
        h.send(Message::Account(make_account(Pubkey::new_unique(), 1, 2000, 1)));
        h.flush();
        assert!(h.try_recv().is_none());
    }

    #[test]
    fn test_dedup_keeps_highest_write_version() {
        let mut h = make_test_harness(1000);
        let pk = Pubkey::new_unique();

        // 3 unique write_versions for slot 1 → normalized to 1*10_000_000 + 1, +2, +3
        // Buffer keeps highest normalized version (10_000_003)
        h.send(Message::Account(make_account(pk, 1, 2000, 1)));
        h.send(Message::Account(make_account(pk, 1, 2000, 3)));
        h.send(Message::Account(make_account(pk, 1, 2000, 2)));
        h.send(make_block_meta(1));
        h.flush();

        if let Some(Message::Account(account)) = h.try_recv() {
            assert_eq!(account.account.write_version, 10_000_003);
        } else {
            panic!("expected Account message");
        }
        assert!(matches!(h.try_recv(), Some(Message::BlockMeta(_))));
        assert!(h.try_recv().is_none());
    }

    #[test]
    fn test_block_meta_flushes_slot() {
        let mut h = make_test_harness(1000);
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();

        h.send(Message::Account(make_account(pk1, 1, 2000, 1)));
        h.send(Message::Account(make_account(pk2, 2, 2000, 1)));
        h.send(make_block_meta(1));
        h.flush();

        if let Some(Message::Account(account)) = h.try_recv() {
            assert_eq!(account.slot, 1);
        } else {
            panic!("expected Account message");
        }
        assert!(matches!(h.try_recv(), Some(Message::BlockMeta(_))));
        assert!(h.try_recv().is_none());
    }

    #[test]
    fn test_shrinking_account_evicts_stale_entry() {
        let mut h = make_test_harness(1000);
        let pk = Pubkey::new_unique();

        h.send(Message::Account(make_account(pk, 1, 2000, 1)));
        h.send(Message::Account(make_account(pk, 1, 500, 2)));
        h.flush();

        if let Some(Message::Account(account)) = h.try_recv() {
            assert_eq!(account.account.data.len(), 500);
            // 2 unique write_versions seen → normalized to 1*10_000_000 + 2
            assert_eq!(account.account.write_version, 10_000_002);
        } else {
            panic!("expected Account message");
        }

        h.send(make_block_meta(1));
        h.flush();

        assert!(matches!(h.try_recv(), Some(Message::BlockMeta(_))));
        assert!(h.try_recv().is_none());
    }

    #[test]
    fn test_non_account_messages_pass_through() {
        let mut h = make_test_harness(1000);

        h.send(Message::Slot(MessageSlot {
            slot: 1,
            parent: Some(0),
            status: MessageSlotStatus::Processed,
            dead_error: None,
            created_at: Timestamp::default(),
        }));
        h.flush();

        assert!(matches!(h.try_recv(), Some(Message::Slot(_))));
    }

    #[test]
    fn test_slot_processed_flushes_buffer() {
        let mut h = make_test_harness(1000);
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();

        // Buffer large accounts in two different slots
        h.send(Message::Account(make_account(pk1, 1, 2000, 1)));
        h.send(Message::Account(make_account(pk2, 2, 2000, 1)));

        // Slot processed for slot 1 should flush only slot 1
        h.send(Message::Slot(MessageSlot {
            slot: 1,
            parent: Some(0),
            status: MessageSlotStatus::Processed,
            dead_error: None,
            created_at: Timestamp::default(),
        }));
        h.flush();

        if let Some(Message::Account(account)) = h.try_recv() {
            assert_eq!(account.slot, 1);
        } else {
            panic!("expected Account message for slot 1");
        }
        assert!(matches!(h.try_recv(), Some(Message::Slot(_))));
        // Slot 2 should still be buffered
        assert!(h.try_recv().is_none());
    }

    #[test]
    fn test_slot_confirmed_does_not_flush_buffer() {
        let mut h = make_test_harness(1000);
        let pk = Pubkey::new_unique();

        h.send(Message::Account(make_account(pk, 1, 2000, 1)));

        // Confirmed status should NOT flush the buffer
        h.send(Message::Slot(MessageSlot {
            slot: 1,
            parent: Some(0),
            status: MessageSlotStatus::Confirmed,
            dead_error: None,
            created_at: Timestamp::default(),
        }));
        h.flush();

        // Only the slot message should pass through, not the buffered account
        if let Some(Message::Slot(slot_msg)) = h.try_recv() {
            assert_eq!(slot_msg.status, MessageSlotStatus::Confirmed);
        } else {
            panic!("expected Slot message");
        }
        assert!(h.try_recv().is_none());
    }

    #[test]
    fn test_slot_processed_dedup_then_flush() {
        let mut h = make_test_harness(1000);
        let pk = Pubkey::new_unique();

        // Send multiple updates for the same account, buffer should keep highest write_version
        // 3 unique write_versions → normalized to 10_000_001, 10_000_002, 10_000_003
        h.send(Message::Account(make_account(pk, 1, 2000, 1)));
        h.send(Message::Account(make_account(pk, 1, 2000, 5)));
        h.send(Message::Account(make_account(pk, 1, 2000, 3)));

        // Flush via slot processed
        h.send(Message::Slot(MessageSlot {
            slot: 1,
            parent: Some(0),
            status: MessageSlotStatus::Processed,
            dead_error: None,
            created_at: Timestamp::default(),
        }));
        h.flush();

        if let Some(Message::Account(account)) = h.try_recv() {
            // Buffer keeps highest normalized write_version
            assert_eq!(account.account.write_version, 10_000_003);
        } else {
            panic!("expected Account message");
        }
        assert!(matches!(h.try_recv(), Some(Message::Slot(_))));
        assert!(h.try_recv().is_none());
    }

    #[test]
    fn test_write_version_normalization() {
        let mut h = make_test_harness(1000);
        let pk1 = Pubkey::new_unique();
        let pk2 = Pubkey::new_unique();

        // pk1 gets 2 updates in slot 5, pk2 gets 1 update
        h.send(Message::Account(make_account(pk1, 5, 100, 100)));
        h.send(Message::Account(make_account(pk1, 5, 100, 200)));
        h.send(Message::Account(make_account(pk2, 5, 100, 300)));
        h.flush();

        // pk1 first update: 5*10_000_000 + 1 = 50_000_001
        let msg1 = h.try_recv().unwrap();
        if let Message::Account(a) = msg1 {
            assert_eq!(a.account.pubkey, pk1);
            assert_eq!(a.account.write_version, 50_000_001);
        } else {
            panic!("expected Account");
        }

        // pk1 second update: 5*10_000_000 + 2 = 50_000_002
        let msg2 = h.try_recv().unwrap();
        if let Message::Account(a) = msg2 {
            assert_eq!(a.account.pubkey, pk1);
            assert_eq!(a.account.write_version, 50_000_002);
        } else {
            panic!("expected Account");
        }

        // pk2 first update: 5*10_000_000 + 1 = 50_000_001
        let msg3 = h.try_recv().unwrap();
        if let Message::Account(a) = msg3 {
            assert_eq!(a.account.pubkey, pk2);
            assert_eq!(a.account.write_version, 50_000_001);
        } else {
            panic!("expected Account");
        }
    }

    #[test]
    fn test_finalized_slot_cleans_tracker() {
        let mut tracker = WriteVersionTracker::new();
        let pk = Pubkey::new_unique();

        // Simulate normalizing accounts in slots 1, 2, 3
        for slot in 1..=3 {
            let account = make_account(pk, slot, 100, 1);
            tracker.normalize(account);
        }
        assert_eq!(tracker.slot_versions.len(), 3);

        // Finalize at slot 3 → slots 1, 2 pruned
        tracker.on_finalized(3);
        assert_eq!(tracker.slot_versions.len(), 1);
        assert!(tracker.slot_versions.contains_key(&3));
    }
}
