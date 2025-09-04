use std::time::Duration;

use clickhouse::{Client, Row};
use serde::Serialize;
use tokio::sync::mpsc;

const DEFAULT_FLUSH_BATCH_SIZE: usize = 500_000;
const DEFAULT_SINK_CHANNEL_SIZE: usize = 10_000_000;
const DEFAULT_FLUSH_INTERVAL: Duration = std::time::Duration::from_millis(500);

/// Trait for data that can be flushed to ClickHouse
pub trait Flushable: Send + 'static {
    /// The row type that will be written to ClickHouse
    type Row: Row + Serialize + Send + Sync + 'static;

    /// The table name this row type writes to
    fn table_name() -> &'static str;

    /// Convert to the actual row type for insertion
    fn to_row(self) -> Self::Row;
}

/// Generic sink for any Flushable type
pub struct Sink<F: Flushable> {
    tx: mpsc::Sender<F>,
}

impl<F: Flushable> Sink<F> {
    /// Create a new sink for a specific flushable type
    pub fn new(client: Client) -> Self {
        let (tx, mut rx) = mpsc::channel(DEFAULT_SINK_CHANNEL_SIZE);

        tokio::spawn(async move {
            let mut batch: Vec<F> = Vec::with_capacity(DEFAULT_FLUSH_BATCH_SIZE);

            let mut flush_interval = tokio::time::interval(DEFAULT_FLUSH_INTERVAL);
            flush_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            let mut monitor_interval = tokio::time::interval(std::time::Duration::from_secs(1));
            monitor_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    Some(flushable) = rx.recv() => {
                        // Convert to row in background thread
                        batch.push(flushable);
                        if batch.len() >= DEFAULT_FLUSH_BATCH_SIZE {
                            flush_rows(&client, &mut batch).await;
                        }
                    }
                    _ = flush_interval.tick() => {
                        if !batch.is_empty() {
                            flush_rows(&client, &mut batch).await;
                        }
                    }
                    _ = monitor_interval.tick() => {
                        let queue_len = rx.len();
                        if queue_len > DEFAULT_SINK_CHANNEL_SIZE / 2 {
                            log::warn!("ClickHouse sink queue growing large: {} items", queue_len);
                        }
                    }
                    else => break,
                }
            }
        });

        Self { tx }
    }

    /// Send a flushable item to the sink
    pub fn send(&self, item: F) -> Result<(), mpsc::error::TrySendError<F>> {
        self.tx.try_send(item)
    }

    /// Get a cloneable sender
    pub fn sender(&self) -> mpsc::Sender<F> {
        self.tx.clone()
    }
}

async fn flush_rows<F: Flushable>(client: &Client, batch: &mut Vec<F>) {
    if batch.is_empty() {
        return;
    }

    let table = F::table_name();
    let batch_size = batch.len();

    let mut insert = match client.insert(table) {
        Ok(i) => i,
        Err(e) => {
            log::error!(
                "Failed to create insert for table '{}', dropping {} rows: {}",
                table,
                batch_size,
                e
            );
            batch.clear();
            return;
        }
    };

    for row in batch.drain(..).map(|f| f.to_row()) {
        if let Err(e) = insert.write(&row).await {
            log::error!(
                "Failed to write row to table '{}', dropping remaining rows: {}",
                table,
                e
            );
            return;
        }
    }

    if let Err(e) = insert.end().await {
        log::error!(
            "Failed to finalize insert for table '{}', batch may be lost: {}",
            table,
            e
        );
    }
}
