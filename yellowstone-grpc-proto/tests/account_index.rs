use yellowstone_grpc_proto::{
    geyser::{subscribe_update::UpdateOneof, SubscribeUpdate},
    prost::Message,
};

#[test]
fn account_transaction_index_wire_presence() {
    for (info, expected) in [
        (vec![], None),
        (vec![0x48, 0], Some(0)),
        (vec![0x48, 42], Some(42)),
        (
            vec![0x48, 255, 255, 255, 255, 255, 255, 255, 255, 255, 1],
            Some(u64::MAX),
        ),
    ] {
        for block in [false, true] {
            let mut nested = vec![if block { 0x5a } else { 0x0a }, info.len() as u8];
            nested.extend_from_slice(&info);
            let mut wire = vec![if block { 0x2a } else { 0x12 }, nested.len() as u8];
            wire.extend(nested);
            let update = SubscribeUpdate::decode(wire.as_slice()).unwrap();
            assert_eq!(update.encode_to_vec(), wire);
            let account = match update.update_oneof.unwrap() {
                UpdateOneof::Account(update) => update.account.unwrap(),
                UpdateOneof::Block(mut update) => update.accounts.remove(0),
                _ => panic!("unexpected update"),
            };
            assert_eq!(account.transaction_index, expected);
        }
    }
}
