//! Event bus: the single place app code emits realtime events.
//!
//! Each emit (1) dispatches to local WebSocket sessions via the [`Hub`],
//! (2) optionally publishes to Redis for other instances, and (3) fans out to
//! matching webhooks. Callers use only [`EventBus::emit_channel`] /
//! [`EventBus::emit_user`].

use crate::db::Db;
use crate::models::ws_protocol::ServerEvent;
use crate::services::webhooks;
use crate::ws::hub::Hub;
use serde::Serialize;
use std::sync::Arc;
use uuid::Uuid;

#[cfg(feature = "redis-pubsub")]
use crate::services::pubsub::PubSub;

/// A serializable wrapper identifying the routing target of a cross-instance
/// event so receivers can re-dispatch correctly.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(tag = "route", rename_all = "snake_case")]
pub enum RoutedEvent {
    Channel { channel_id: Uuid, event: ServerEvent },
    User { user_id: Uuid, event: ServerEvent },
}

#[derive(Clone)]
pub struct EventBus {
    inner: Arc<EventBusInner>,
}

struct EventBusInner {
    hub: Hub,
    db: Db,
    #[cfg(feature = "redis-pubsub")]
    pubsub: Option<PubSub>,
}

impl EventBus {
    #[cfg(feature = "redis-pubsub")]
    pub fn new(hub: Hub, db: Db, pubsub: Option<PubSub>) -> Self {
        Self {
            inner: Arc::new(EventBusInner { hub, db, pubsub }),
        }
    }

    #[cfg(not(feature = "redis-pubsub"))]
    pub fn new(hub: Hub, db: Db) -> Self {
        Self {
            inner: Arc::new(EventBusInner { hub, db }),
        }
    }

    pub fn hub(&self) -> &Hub {
        &self.inner.hub
    }

    /// Emit an event scoped to a channel. Delivered to all subscribers across
    /// all instances, and to any channel-scoped webhooks.
    pub fn emit_channel(&self, channel_id: Uuid, event: ServerEvent) {
        self.inner.hub.dispatch_channel(channel_id, &event);

        #[cfg(feature = "redis-pubsub")]
        if let Some(ps) = &self.inner.pubsub {
            ps.publish(RoutedEvent::Channel {
                channel_id,
                event: event.clone(),
            });
        }

        self.spawn_webhooks(Some(channel_id), &event);
    }

    /// Emit an event scoped to a single user across all their sessions/instances.
    pub fn emit_user(&self, user_id: Uuid, event: ServerEvent) {
        self.inner.hub.dispatch_user(user_id, &event);

        #[cfg(feature = "redis-pubsub")]
        if let Some(ps) = &self.inner.pubsub {
            ps.publish(RoutedEvent::User {
                user_id,
                event: event.clone(),
            });
        }
    }

    /// Re-dispatch an event that arrived from another instance via Redis.
    /// Does NOT republish (avoids loops) and does NOT re-fire webhooks (the
    /// originating instance already did).
    pub fn dispatch_remote(&self, routed: RoutedEvent) {
        match routed {
            RoutedEvent::Channel { channel_id, event } => {
                self.inner.hub.dispatch_channel(channel_id, &event)
            }
            RoutedEvent::User { user_id, event } => self.inner.hub.dispatch_user(user_id, &event),
        }
    }

    /// Fire webhook deliveries in the background so request latency is unaffected.
    fn spawn_webhooks(&self, channel_id: Option<Uuid>, event: &ServerEvent) {
        let Some((name, payload)) = webhooks::event_descriptor(event) else {
            return;
        };
        let db = self.inner.db.clone();
        tokio::spawn(async move {
            if let Err(e) = webhooks::deliver(&db, channel_id, name, payload).await {
                tracing::warn!(error = %e, "webhook delivery failed");
            }
        });
    }
}
