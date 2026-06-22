//! Optional Redis pub/sub bridge for multi-instance scaling.
//!
//! Each instance publishes routed events to a shared channel and runs a
//! background subscriber that re-dispatches inbound events to its local
//! sessions. Enabled only when `REDIS_URL` is configured and the
//! `redis-pubsub` feature is on.

#![cfg(feature = "redis-pubsub")]

use crate::services::events::{EventBus, RoutedEvent};
use redis::AsyncCommands;
use tokio::sync::mpsc;

const CHANNEL: &str = "rsmc_engine:events";

/// Handle used by [`EventBus`] to publish events to peers.
#[derive(Clone)]
pub struct PubSub {
    tx: mpsc::UnboundedSender<RoutedEvent>,
}

impl PubSub {
    /// Connect to Redis and start the publisher task. The subscriber side is
    /// started later via [`spawn_subscriber`] once the [`EventBus`] exists.
    pub async fn connect(url: &str) -> anyhow::Result<(Self, redis::Client)> {
        let client = redis::Client::open(url)?;
        // Verify connectivity early.
        let mut conn = client.get_multiplexed_async_connection().await?;
        let _: String = redis::cmd("PING").query_async(&mut conn).await?;

        let (tx, mut rx) = mpsc::unbounded_channel::<RoutedEvent>();

        // Publisher task: serialize and PUBLISH each outgoing event.
        let pub_conn = client.get_multiplexed_async_connection().await?;
        tokio::spawn(async move {
            let mut conn = pub_conn;
            while let Some(evt) = rx.recv().await {
                match serde_json::to_string(&evt) {
                    Ok(payload) => {
                        if let Err(e) = conn.publish::<_, _, ()>(CHANNEL, payload).await {
                            tracing::warn!(error = %e, "redis publish failed");
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "failed to serialize routed event"),
                }
            }
        });

        tracing::info!("redis pub/sub connected");
        Ok((Self { tx }, client))
    }

    /// Queue an event for publication to peer instances (non-blocking).
    pub fn publish(&self, event: RoutedEvent) {
        let _ = self.tx.send(event);
    }
}

/// Start the subscriber loop that re-dispatches peer events locally.
pub fn spawn_subscriber(client: redis::Client, bus: EventBus) {
    tokio::spawn(async move {
        loop {
            match run_subscriber(&client, &bus).await {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "redis subscriber dropped; reconnecting in 2s");
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            }
        }
    });
}

async fn run_subscriber(client: &redis::Client, bus: &EventBus) -> anyhow::Result<()> {
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(CHANNEL).await?;
    tracing::info!("redis subscriber listening");

    let mut stream = pubsub.on_message();
    use futures::StreamExt;
    while let Some(msg) = stream.next().await {
        let payload: String = msg.get_payload()?;
        match serde_json::from_str::<RoutedEvent>(&payload) {
            Ok(evt) => bus.dispatch_remote(evt),
            Err(e) => tracing::warn!(error = %e, "bad routed event payload"),
        }
    }
    Ok(())
}
