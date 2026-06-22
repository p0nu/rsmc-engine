//! Shared application state injected into every handler via Axum's `State`.

use crate::auth::JwtKeys;
use crate::config::Settings;
use crate::db::Db;
use crate::services::events::EventBus;
use crate::ws::hub::Hub;
use std::sync::Arc;

/// Cheaply-cloneable handle to all shared services.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<AppStateInner>,
}

pub struct AppStateInner {
    pub db: Db,
    pub config: Settings,
    pub jwt: JwtKeys,
    pub hub: Hub,
    pub events: EventBus,
}

impl AppState {
    pub fn new(db: Db, config: Settings, jwt: JwtKeys, hub: Hub, events: EventBus) -> Self {
        Self {
            inner: Arc::new(AppStateInner {
                db,
                config,
                jwt,
                hub,
                events,
            }),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = AppStateInner;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
