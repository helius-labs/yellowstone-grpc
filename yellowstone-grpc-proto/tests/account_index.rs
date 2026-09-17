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
fn account_transaction_index_manual_encoder_parity() {
    for transaction_index in [None, Some(0), Some(42), Some(u64::MAX)] {
        for ranges in [vec![], vec![Range { start: 2, end: 5 }]] {
            let account = MessageAccountInfo::from_update_oneof(SubscribeUpdateAccountInfo {
                pubkey: vec![1; 32],
                owner: vec![2; 32],
                data: vec![42; 16],
                transaction_index,
                ..Default::default()
            })
            .unwrap();
            assert_eq!(account.transaction_index, transaction_index);
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
            assert_eq!(update.account.unwrap().transaction_index, transaction_index);
        }
    }
}
