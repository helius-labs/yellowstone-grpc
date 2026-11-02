#![cfg(feature = "plugin")]

use {
    laserstream_core_proto::{
        geyser::{SubscribeRequest, SubscribeRequestFilterBlockFooter, SubscribeUpdateBlockFooter},
        plugin::{
            filter::{limits::FilterLimits, name::FilterNames, Filter},
            message::{Message, MessageBlockFooter},
        },
        prost::Message as _,
        prost_types::Timestamp,
    },
    serde_json::Value,
    std::{sync::Arc, time::Duration},
};

// Generated from rpcpool/yellowstone-grpc 43a827c3 (Triton's own filter + encoder).
const GOLDEN: &str = include_str!("../fixtures/triton-footer-golden.json");

#[test]
fn triton_block_footer_golden_vectors() {
    let golden: Value = serde_json::from_str(GOLDEN).unwrap();
    let created_at = Timestamp {
        seconds: golden["created_at"]["seconds"].as_i64().unwrap(),
        nanos: golden["created_at"]["nanos"].as_i64().unwrap() as i32,
    };
    let vectors = golden["vectors"].as_object().unwrap();
    assert_eq!(vectors.len(), 3);
    for (name, vector) in vectors {
        let footer_bytes = hex::decode(vector["footer"].as_str().unwrap()).unwrap();
        let footer = SubscribeUpdateBlockFooter::decode(footer_bytes.as_slice()).unwrap();
        assert_eq!(
            footer.encode_to_vec(),
            footer_bytes,
            "{name}: footer re-encode"
        );
        let message = Message::BlockFooter(Arc::new(MessageBlockFooter {
            block_footer: footer,
            created_at,
        }));

        for (key, include_certificates) in
            [("null", None), ("false", Some(false)), ("true", Some(true))]
        {
            let expected = vector["updates"][key]
                .as_array()
                .unwrap()
                .iter()
                .map(|hex| hex.as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            let actual = encoded_updates(&message, &[("f", include_certificates)]);
            assert_eq!(actual, expected, "{name}: include_certificates={key}");
        }

        let expected = vector["mixed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|update| update["hex"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        let actual = encoded_updates(&message, &[("a", None), ("b", Some(true))]);
        assert_eq!(actual, expected, "{name}: mixed");
    }
}

fn encoded_updates(message: &Message, filters: &[(&str, Option<bool>)]) -> Vec<String> {
    let request = SubscribeRequest {
        block_footer: filters
            .iter()
            .map(|(name, include_certificates)| {
                (
                    (*name).to_owned(),
                    SubscribeRequestFilterBlockFooter {
                        include_certificates: *include_certificates,
                    },
                )
            })
            .collect(),
        ..Default::default()
    };
    let mut names = FilterNames::new(64, 1024, Duration::from_secs(1));
    let filter = Filter::new(&request, &FilterLimits::default(), &mut names).unwrap();
    filter
        .get_updates(message, None)
        .into_iter()
        .map(|update| hex::encode(update.encode_to_vec()))
        .collect()
}
