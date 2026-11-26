//! Executable entrypoint for the rsmc-engine server.
//!
//! Thin by design: load config, init tracing, delegate wiring to
//! [`rsmc_engine::bootstrap`], then bind and serve with graceful shutdown.

use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing_subscriber::{prelude::*, EnvFilter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Load `.env` if present (ignored in production where real env vars win).
    let _ = dotenvy::dotenv();

    let settings = rsmc_engine::config::Settings::load()?;
    init_tracing(&settings.logging);

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting rsmc-engine"
    );

    // Ensure the upload directory exists AND is writable before accepting
    // traffic. A silent failure here is the classic cause of "uploads don't
    // work" (wrong permissions / read-only location), so fail loudly at boot
    // with an actionable message instead of surfacing generic 500s per upload.
    let upload_dir = &settings.storage.upload_dir;
    tokio::fs::create_dir_all(upload_dir).await.map_err(|e| {
        anyhow::anyhow!("cannot create upload directory '{upload_dir}': {e}. Check the path and that the process user can write to it.")
    })?;
    let probe = std::path::Path::new(upload_dir).join(".rsmc-write-test");
    tokio::fs::write(&probe, b"ok").await.map_err(|e| {
        anyhow::anyhow!("upload directory '{upload_dir}' is not writable: {e}. Fix its permissions (in Docker, ensure the mounted volume is writable by the container user).")
    })?;
    let _ = tokio::fs::remove_file(&probe).await;

    let bind = settings.bind_addr();
    let state = rsmc_engine::bootstrap(settings).await?;
    let app = rsmc_engine::build_router(state);

    let addr: SocketAddr = bind.parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(%addr, "listening");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;

    tracing::info!("serve loop returned; shutdown complete");
    Ok(())
}

/// Init tracing: honor `RUST_LOG`, else configured level; `json` or pretty output.
fn init_tracing(cfg: &rsmc_engine::config::LoggingConfig) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.level));

    let registry = tracing_subscriber::registry().with(filter);

    if cfg.format.eq_ignore_ascii_case("json") {
        registry.with(tracing_subscriber::fmt::layer().json()).init();
    } else {
        registry.with(tracing_subscriber::fmt::layer()).init();
    }
}

/// Resolve on Ctrl-C (or SIGTERM on Unix) for a graceful drain.
async fn shutdown_signal() {
    // If no handler can be installed (e.g. no TTY in a container), don't treat
    // that as "shut down" - exiting 0 under a restart policy would loop forever.
    // Fall back to a never-resolving future and wait for a real signal.
    let ctrl_c = async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => {}
            Err(err) => {
                tracing::warn!(%err, "ctrl-c handler unavailable; ignoring");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(err) => {
                tracing::warn!(%err, "SIGTERM handler unavailable; ignoring");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("shutdown signal received");
}
