//! WebSocket endpoint: authenticates, registers a hub session, and pumps
//! events bidirectionally.
//!
//! Auth: the access token is supplied via the `?token=` query parameter (the
//! browser WebSocket API cannot set Authorization headers).

use crate::auth::TokenKind;
use crate::models::ws_protocol::{ClientEvent, ServerEvent};
use crate::services::access;
use crate::state::AppState;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        Query, State, WebSocketUpgrade,
    },
    response::Response,
};
use chrono::Utc;
use serde::Deserialize;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct WsAuth {
    pub token: String,
}

/// `GET /ws?token=<access_token>`
pub async fn ws_handler(
    State(state): State<AppState>,
    Query(auth): Query<WsAuth>,
    upgrade: WebSocketUpgrade,
) -> Response {
    // Validate before upgrading so failures return a clean HTTP error.
    let claims = match state.jwt.verify(&auth.token, TokenKind::Access) {
        Ok(c) => c,
        Err(_) => {
            return axum::response::IntoResponse::into_response((
                axum::http::StatusCode::UNAUTHORIZED,
                "invalid token",
            ))
        }
    };
    let user_id = claims.sub;
    upgrade.on_upgrade(move |socket| handle_socket(socket, state, user_id))
}

async fn handle_socket(socket: WebSocket, state: AppState, user_id: Uuid) {
    let (mut sink, mut stream) = {
        use futures::StreamExt;
        socket.split()
    };

    // Outbound channel: hub -> this socket.
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerEvent>();
    let (session_id, became_online) = state.events.hub().register(user_id, tx);

    if became_online {
        mark_presence(&state, user_id, true).await;
    }

    // Task A: forward hub events to the client socket.
    let send_task = tokio::spawn(async move {
        use futures::SinkExt;
        while let Some(event) = rx.recv().await {
            match serde_json::to_string(&event) {
                Ok(text) => {
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(e) => tracing::warn!(error = %e, "failed to encode server event"),
            }
        }
    });

    // Task B: read client frames and act on them.
    let recv_state = state.clone();
    let recv_task = tokio::spawn(async move {
        use futures::StreamExt;
        while let Some(Ok(msg)) = stream.next().await {
            match msg {
                Message::Text(text) => {
                    handle_client_frame(&recv_state, user_id, session_id, &text).await;
                }
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) | Message::Binary(_) => {}
            }
        }
    });

    // When either side ends, tear down.
    tokio::select! {
        _ = send_task => {},
        _ = recv_task => {},
    }

    let now_offline = state.events.hub().unregister(session_id);
    if now_offline {
        mark_presence(&state, user_id, false).await;
    }
}

async fn handle_client_frame(state: &AppState, user_id: Uuid, session_id: Uuid, text: &str) {
    let event: ClientEvent = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(_) => {
            state.events.hub().dispatch_user(
                user_id,
                &ServerEvent::Error {
                    message: "malformed client event".into(),
                },
            );
            return;
        }
    };

    match event {
        ClientEvent::Subscribe { channel_id } => {
            // Only allow subscribing to channels the user belongs to.
            if access::require_member(&state.db, channel_id, user_id)
                .await
                .is_ok()
            {
                state.events.hub().subscribe(session_id, channel_id);
            } else {
                state.events.hub().dispatch_user(
                    user_id,
                    &ServerEvent::Error {
                        message: "cannot subscribe: not a member".into(),
                    },
                );
            }
        }
        ClientEvent::Unsubscribe { channel_id } => {
            state.events.hub().unsubscribe(session_id, channel_id);
        }
        ClientEvent::Typing { channel_id } => {
            // Broadcast a transient typing indicator to the channel.
            if access::require_member(&state.db, channel_id, user_id)
                .await
                .is_ok()
            {
                state
                    .events
                    .emit_channel(channel_id, ServerEvent::Typing { channel_id, user_id });
            }
        }
        ClientEvent::Ping => {
            state
                .events
                .hub()
                .dispatch_user(user_id, &ServerEvent::Pong);
        }
    }
}

/// Persist presence and broadcast the change.
async fn mark_presence(state: &AppState, user_id: Uuid, online: bool) {
    let now = Utc::now();
    let _ = sqlx::query(
        r#"
        INSERT INTO presence (user_id, online, last_seen)
        VALUES ($1, $2, $3)
        ON CONFLICT (user_id) DO UPDATE SET online = $2, last_seen = $3
        "#,
    )
    .bind(user_id)
    .bind(online)
    .bind(now)
    .execute(&state.db)
    .await;

    // Broadcast to the user's own sessions and peers via the event bus.
    state.events.emit_user(
        user_id,
        ServerEvent::Presence {
            user_id,
            online,
            last_seen: now,
        },
    );
}
