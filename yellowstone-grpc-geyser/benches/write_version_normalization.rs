use {
    criterion::{criterion_group, criterion_main, BenchmarkId, Criterion},
    prost_types::Timestamp,
    solana_sdk::pubkey::Pubkey,
    std::sync::Arc,
    yellowstone_grpc_geyser::account_buffer::WriteVersionTracker,
    yellowstone_grpc_proto::plugin::message::{MessageAccount, MessageAccountInfo},
};

fn make_account(pubkey: Pubkey, slot: u64, write_version: u64) -> MessageAccount {
    MessageAccount {
        account: Arc::new(MessageAccountInfo {
            pubkey,
            lamports: 1,
            owner: Pubkey::default(),
            executable: false,
            rent_epoch: 0,
            data: vec![0u8; 128],
            write_version,
            txn_signature: None,
        }),
        slot,
        is_startup: false,
        created_at: Timestamp::default(),
    }
}

fn bench_normalize_single(c: &mut Criterion) {
    c.bench_function("normalize_single_account", |b| {
        b.iter_batched(
            || {
                let tracker = WriteVersionTracker::new();
                let account = make_account(Pubkey::new_unique(), 1, 42);
                (tracker, account)
            },
            |(mut tracker, account)| tracker.normalize(account),
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_normalize_with_populated_slot(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_with_existing_accounts");

    for num_accounts in [100, 1_000, 10_000] {
        group.bench_with_input(
            BenchmarkId::new("accounts_in_slot", num_accounts),
            &num_accounts,
            |b, &n| {
                b.iter_batched(
                    || {
                        let mut tracker = WriteVersionTracker::new();
                        // Pre-populate with n unique pubkeys in slot 1
                        for i in 0..n {
                            let account = make_account(Pubkey::new_unique(), 1, i as u64);
                            tracker.normalize(account);
                        }
                        let new_account = make_account(Pubkey::new_unique(), 1, 999);
                        (tracker, new_account)
                    },
                    |(mut tracker, account)| tracker.normalize(account),
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_normalize_repeated_pubkey(c: &mut Criterion) {
    let mut group = c.benchmark_group("normalize_repeated_pubkey_updates");

    for num_updates in [1, 10, 100, 1_000] {
        group.bench_with_input(
            BenchmarkId::new("prior_updates", num_updates),
            &num_updates,
            |b, &n| {
                b.iter_batched(
                    || {
                        let mut tracker = WriteVersionTracker::new();
                        let pk = Pubkey::new_unique();
                        // Pre-populate with n updates for the same pubkey
                        for i in 0..n {
                            let account = make_account(pk, 1, i as u64);
                            tracker.normalize(account);
                        }
                        let new_account = make_account(pk, 1, n as u64);
                        (tracker, new_account)
                    },
                    |(mut tracker, account)| tracker.normalize(account),
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_normalize_single,
    bench_normalize_with_populated_slot,
    bench_normalize_repeated_pubkey,
);
criterion_main!(benches);
