use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use once_cell::sync::Lazy;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_rpc_client_api::{client_error, request};
use tokio::time::interval;

pub static SHOULD_DISCONNECT: Lazy<Arc<AtomicBool>> =
    Lazy::new(|| Arc::new(AtomicBool::new(false)));

pub async fn is_node_healthy(client: &RpcClient) -> bool {
    loop {
        match client.get_health().await {
            Ok(()) => return true,
            Err(err) => {
                if let client_error::ErrorKind::RpcError(request::RpcError::RpcResponseError {
                    code: _,
                    message: _,
                    data: request::RpcResponseErrorData::NodeUnhealthy { .. },
                }) = &err.kind
                {
                    return false;
                } else {
                    log::error!("Failed to get health: {}", err);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            }
        }
    }
}

pub async fn run_forced_disconnection_monitor(rpc_client: RpcClient) {
    let mut interval = interval(Duration::from_millis(100));
    loop {
        interval.tick().await;
        let is_healthy = !is_node_healthy(&rpc_client).await;
        SHOULD_DISCONNECT.store(is_healthy, Ordering::SeqCst);
    }
}
