use crate::sink::{Flushable, Sink};
use clickhouse::{Client, Row};
use once_cell::sync::OnceCell;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use time::OffsetDateTime;
use tokio::sync::mpsc;

#[derive(Debug, Serialize, Deserialize, Row, Clone)]
pub struct EventRow {
    #[serde(with = "clickhouse::serde::time::datetime64::micros")]
    pub timestamp: OffsetDateTime,
    pub data: String,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub timestamp: OffsetDateTime,
    pub data: serde_json::Value, // Unserialized JSON
}

impl Flushable for Event {
    type Row = EventRow;

    fn table_name() -> &'static str {
        "events"
    }

    fn to_row(self) -> Self::Row {
        EventRow {
            timestamp: self.timestamp,
            data: self.data.to_string(), // Serialize in background thread
        }
    }
}

static EVENT_TX: OnceCell<mpsc::Sender<Event>> = OnceCell::new();
static EVENT_CONTEXT: OnceCell<EventContext> = OnceCell::new();

thread_local! {
    static CACHED_TX: RefCell<Option<mpsc::Sender<Event>>> = RefCell::new(None);
}

#[derive(Clone, Debug)]
struct EventContext {
    env: Option<String>,
    host: Option<String>,
}

/// Initialize the event recording system
pub async fn init(client: Client, env: Option<String>, host: Option<String>) -> anyhow::Result<()> {
    let sink = Sink::<Event>::new(client);
    EVENT_TX
        .set(sink.sender())
        .expect("Event sender already initialized");

    EVENT_CONTEXT
        .set(EventContext { env, host })
        .expect("Event context already initialized");

    log::info!("Event recording initialized");
    Ok(())
}

/// Record an event with JSON data
pub fn record(data: serde_json::Value) {
    record_with_time(data, OffsetDateTime::now_utc());
}

/// Record an event with JSON data and specific timestamp
pub fn record_with_time(mut data: serde_json::Value, timestamp: OffsetDateTime) {
    if let Some(context) = EVENT_CONTEXT.get() {
        if let Some(obj) = data.as_object_mut() {
            if let Some(env) = &context.env {
                obj.insert("env".to_string(), serde_json::Value::String(env.clone()));
            }
            if let Some(host) = &context.host {
                obj.insert("host".to_string(), serde_json::Value::String(host.clone()));
            }
        }
    }

    CACHED_TX.with(|cache| {
        let mut cache = cache.borrow_mut();

        let tx = match cache.as_ref() {
            Some(tx) => tx,
            None => {
                if let Some(tx) = EVENT_TX.get() {
                    *cache = Some(tx.clone());
                    cache.as_ref().unwrap()
                } else {
                    return; // Not initialized
                }
            }
        };

        let event = Event { timestamp, data };

        // Try send, ignore if channel is full or closed
        let _ = tx.try_send(event);
    });
}
