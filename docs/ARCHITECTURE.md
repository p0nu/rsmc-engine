# Architecture

```
                       ┌──────────────────────────────┐
   HTTP / WebSocket    │          Axum router         │
   ───────────────────►│   handlers/  middleware/     │
                       └───────────────┬──────────────┘
                                       │ AppState (db, jwt, hub, events)
              ┌────────────────────────┼─────────────────────────┐
              ▼                        ▼                         ▼
        ┌───────────┐          ┌──────────────┐          ┌──────────────┐
        │  SQLx /   │          │   ws::Hub    │          │  EventBus    │
        │ Postgres  │          │ (live socket │◄────────►│ local + redis│
        │           │          │  sessions)   │          │ + webhooks   │
        └───────────┘          └──────────────┘          └──────┬───────┘
                                                                │
                                              outbound HTTP ────┘ (HMAC-signed)
```

- **`handlers/`** — one module per resource; thin, returning typed `AppResult`.
- **`middleware/`** — extractors for authenticated users (`AuthUser`,
  `AdminUser`) and request validation (`ValidatedJson`).
- **`services/`** — cross-cutting logic: access checks, the event bus,
  notifications, webhook signing/delivery, and the Redis pub/sub bridge.
- **`ws/`** — the WebSocket handler and the in-process `Hub` that holds live
  sessions and fans events to subscribers.
- **`models/`** — domain types, request/response DTOs, and the WS protocol enums.
- **`error/`** — a single `AppError` mapping cleanly to HTTP status + JSON.

When `REDIS_URL` is set, the event bus publishes every realtime event to Redis
and each instance re-dispatches to its own local sockets, so you can run many
stateless replicas behind a load balancer.

## Project layout

```
src/
  config/      layered settings loader
  db/          pool + migration runner
  auth/        JWT issuing/verification, password hashing
  models/      domain types, DTOs, WS protocol
  handlers/    REST + WS route handlers
  middleware/  auth extractors, request validation
  services/    access control, event bus, notifications, webhooks, pubsub
  ws/          WebSocket handler + session hub
  error/       AppError + IntoResponse
  state.rs     shared AppState
  lib.rs       router factory + bootstrap
  main.rs      executable entrypoint
migrations/    SQL schema (applied via sqlx::migrate!)
docs/          API.md, WEBSOCKET.md, SCHEMA.md, INTEGRATION.md
tests/         in-process integration tests
```

## Embedding in your own binary

The crate exposes a router factory and a one-call bootstrap, so you can mount the
engine inside a larger Axum app or customize startup:

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let settings = rsmc_engine::config::Settings::load()?;
    let state = rsmc_engine::bootstrap(settings).await?;
    let app = rsmc_engine::build_router(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
```
