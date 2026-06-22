//! In-process realtime hub: fan-out point for events.
//!
//! Each WebSocket registers a session; the hub tracks who's online and
//! broadcasts [`ServerEvent`]s to relevant sessions. Self-contained for single
//! instances; [`crate::services::pubsub`] bridges events across instances.

use crate::models::ws_protocol::ServerEvent;
use dashmap::DashMap;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

/// A per-connection outbound channel sender.
pub type SessionTx = mpsc::UnboundedSender<ServerEvent>;

/// A single connected session belonging to a user.
struct Session {
    user_id: Uuid,
    tx: SessionTx,
    /// Channels this session is currently subscribed to.
    subscriptions: HashSet<Uuid>,
}

/// The shared realtime hub. Cheap to clone (`Arc` inside).
#[derive(Clone)]
pub struct Hub {
    inner: Arc<HubInner>,
}

struct HubInner {
    /// session_id -> Session
    sessions: DashMap<Uuid, Session>,
    /// user_id -> set of session_ids (a user may have many devices/tabs).
    user_sessions: DashMap<Uuid, HashSet<Uuid>>,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HubInner {
                sessions: DashMap::new(),
                user_sessions: DashMap::new(),
            }),
        }
    }

    /// Register a new session. Returns the session id and whether this is the
    /// user's first active session (i.e. they just came online).
    pub fn register(&self, user_id: Uuid, tx: SessionTx) -> (Uuid, bool) {
        let session_id = Uuid::new_v4();
        self.inner.sessions.insert(
            session_id,
            Session {
                user_id,
                tx,
                subscriptions: HashSet::new(),
            },
        );

        let mut entry = self.inner.user_sessions.entry(user_id).or_default();
        let first = entry.is_empty();
        entry.insert(session_id);
        (session_id, first)
    }

    /// Remove a session. Returns true if the user has no more sessions (went
    /// offline).
    pub fn unregister(&self, session_id: Uuid) -> bool {
        let Some((_, session)) = self.inner.sessions.remove(&session_id) else {
            return false;
        };
        let mut now_offline = false;
        if let Some(mut set) = self.inner.user_sessions.get_mut(&session.user_id) {
            set.remove(&session_id);
            now_offline = set.is_empty();
        }
        if now_offline {
            self.inner.user_sessions.remove(&session.user_id);
        }
        now_offline
    }

    pub fn subscribe(&self, session_id: Uuid, channel_id: Uuid) {
        if let Some(mut s) = self.inner.sessions.get_mut(&session_id) {
            s.subscriptions.insert(channel_id);
        }
    }

    pub fn unsubscribe(&self, session_id: Uuid, channel_id: Uuid) {
        if let Some(mut s) = self.inner.sessions.get_mut(&session_id) {
            s.subscriptions.remove(&channel_id);
        }
    }

    pub fn is_online(&self, user_id: Uuid) -> bool {
        self.inner
            .user_sessions
            .get(&user_id)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    }

    pub fn online_count(&self) -> usize {
        self.inner.user_sessions.len()
    }

    /// Deliver an event to every local session subscribed to `channel_id`.
    pub fn dispatch_channel(&self, channel_id: Uuid, event: &ServerEvent) {
        for s in self.inner.sessions.iter() {
            if s.subscriptions.contains(&channel_id) {
                let _ = s.tx.send(event.clone());
            }
        }
    }

    /// Deliver an event to every local session of a specific user.
    pub fn dispatch_user(&self, user_id: Uuid, event: &ServerEvent) {
        if let Some(set) = self.inner.user_sessions.get(&user_id) {
            for session_id in set.iter() {
                if let Some(s) = self.inner.sessions.get(session_id) {
                    let _ = s.tx.send(event.clone());
                }
            }
        }
    }
}
