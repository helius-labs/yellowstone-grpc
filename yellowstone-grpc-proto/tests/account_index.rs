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
use std::sync::Arc;

#[test]
fn native_v3_write_has_no_transaction() {
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
        laserstream_core_proto::AccountIndex::from_wire(account.transaction_index),
        laserstream_core_proto::AccountIndex::NoTransaction
    );
}

#[test]
fn account_index_manual_encoder_parity() {
    for transaction_index in [0, 42, u64::MAX, u64::MAX - 1] {
        let account = MessageAccountInfo::from_update_oneof(SubscribeUpdateAccountInfo {
            pubkey: vec![1; 32],
            owner: vec![2; 32],
            transaction_index,

            ..Default::default()
        })
        .unwrap();
        let message = MessageAccount {
            bank_id: None,
            account: Arc::new(account),
            slot: 42,
            is_startup: false,
            created_at: Default::default(),
        };
        let filtered = FilteredUpdate::new_empty(FilteredUpdateOneof::account(
            &message,
            FilterAccountsDataSlice::default(),
        ));
        let expected = filtered.as_subscribe_update();
        assert_eq!(filtered.encode_to_vec(), expected.encode_to_vec());
        assert_eq!(filtered.encoded_len(), expected.encoded_len());
        let decoded = SubscribeUpdate::decode(filtered.encode_to_vec().as_slice()).unwrap();
        let Some(UpdateOneof::Account(update)) = decoded.update_oneof else {
            panic!("account expected")
        };
        let account = update.account.unwrap();
        assert_eq!(account.transaction_index, transaction_index);
    }
}
