use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use once_cell::sync::Lazy;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::{client_error, request};
use tokio::time::interval;

pub const HEALTH_CHECK_SLOT_DISTANCE: u64 = 100;
pub static NUM_SLOTS_BEHIND: Lazy<Arc<AtomicU64>> = Lazy::new(|| Arc::new(AtomicU64::new(0)));

pub async fn fetch_node_blocks_behind_with_infinite_retry(client: &RpcClient) -> u64 {
    loop {
        match client.get_health().await {
            Ok(()) => {
                return 0;
            }
            Err(err) => {
                if let client_error::ErrorKind::RpcError(request::RpcError::RpcResponseError {
                    code: _,
                    message: _,
                    data: request::RpcResponseErrorData::NodeUnhealthy { num_slots_behind },
                }) = &err.kind
                {
                    return num_slots_behind.unwrap_or(2000);
                } else {
                    log::error!("Failed to get health: {}", err);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        }
    }
}

pub async fn keep_track_of_node_health(rpc_client: RpcClient) {
    let mut interval = interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        let blocks_behind = fetch_node_blocks_behind_with_infinite_retry(&rpc_client).await;
        NUM_SLOTS_BEHIND.store(blocks_behind, Ordering::SeqCst);
    }
}
