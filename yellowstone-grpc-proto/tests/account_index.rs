#![cfg(feature = "plugin")]

use laserstream_core_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeUpdate, SubscribeUpdateAccountInfo},
    plugin::{
        filter::{
            message::{FilteredUpdate, FilteredUpdateOneof},
            FilterAccountsDataSlice,
        },
        message::{MessageAccount, MessageAccountInfo},
    },
    prost::Message,
};
use std::{ops::Range, sync::Arc};

#[test]
fn native_v3_write_defaults_to_native_operation_zero() {
    let info = agave_geyser_plugin_interface::geyser_plugin_interface::ReplicaAccountInfoV3 {
        pubkey: &[1; 32],
        lamports: 1,
        owner: &[2; 32],
        executable: false,
        rent_epoch: 0,
        data: &[],
        write_version: 1,
        txn: None,
    };
    let account = MessageAccountInfo::from_geyser(&info);
    assert_eq!(
        laserstream_core_proto::AccountIndex::from_wire(
            account.transaction_index,
            account.native_operation_count
        ),
        laserstream_core_proto::AccountIndex::NativeOperation(0)
    );
}

#[test]
fn account_index_manual_encoder_parity() {
    for (transaction_index, native_operation_count) in [
        (0, 0),
        (42, 9),
        (u64::MAX, 0),
        (u64::MAX, 7),
        (u64::MAX, u64::MAX),
        (u64::MAX - 1, 0),
    ] {
        for ranges in [vec![], vec![Range { start: 2, end: 5 }]] {
            let account = MessageAccountInfo::from_update_oneof(SubscribeUpdateAccountInfo {
                pubkey: vec![1; 32],
                owner: vec![2; 32],
                data: vec![42; 16],
                transaction_index,
                native_operation_count,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(account.transaction_index, transaction_index);
            assert_eq!(account.native_operation_count, native_operation_count);
            let message = MessageAccount {
                account: Arc::new(account),
                slot: 42,
                is_startup: false,
                created_at: Default::default(),
            };
            let slices = FilterAccountsDataSlice::new_unchecked(Arc::new(ranges));
            let filtered =
                FilteredUpdate::new_empty(FilteredUpdateOneof::account(&message, slices));
            let expected = filtered.as_subscribe_update();
            assert_eq!(filtered.encode_to_vec(), expected.encode_to_vec());
            assert_eq!(filtered.encoded_len(), expected.encoded_len());
            let decoded = SubscribeUpdate::decode(filtered.encode_to_vec().as_slice()).unwrap();
            let Some(UpdateOneof::Account(update)) = decoded.update_oneof else {
                panic!("account expected")
            };
            let account = update.account.unwrap();
            assert_eq!(account.transaction_index, transaction_index);
            assert_eq!(account.native_operation_count, native_operation_count);
        }
    }
}
