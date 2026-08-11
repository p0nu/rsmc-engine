//! Application configuration.
//!
//! Layered: defaults → optional `config.toml` → `APP__`-prefixed env vars
//! (e.g. `APP__SERVER__PORT=9000`). A few common bare vars (`DATABASE_URL`,
//! `JWT_SECRET`) are also read directly for twelve-factor deployments.

use serde::Deserialize;
use std::time::Duration;

/// Top-level configuration object passed around the application.
#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub redis: RedisConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub rate_limit: RateLimitConfig,
    #[serde(default)]
    pub email: EmailConfig,
    #[serde(default)]
    pub retention: RetentionConfig,
    #[serde(default)]
    pub gdpr: GdprConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Allowed CORS origins. Use `["*"]` to allow any (dev only).
    #[serde(default = "default_cors")]
    pub cors_origins: Vec<String>,
    /// Maximum request body size in bytes (default 25 MiB).
    #[serde(default = "default_body_limit")]
    pub max_body_bytes: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            cors_origins: default_cors(),
            max_body_bytes: default_body_limit(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub url: String,
    #[serde(default = "default_max_conns")]
    pub max_connections: u32,
    #[serde(default = "default_min_conns")]
    pub min_connections: u32,
    #[serde(default = "default_acquire_timeout_secs")]
    pub acquire_timeout_secs: u64,
    /// Run migrations automatically on startup.
    #[serde(default = "default_true")]
    pub auto_migrate: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            max_connections: default_max_conns(),
            min_connections: default_min_conns(),
            acquire_timeout_secs: default_acquire_timeout_secs(),
            auto_migrate: default_true(),
        }
    }
}

impl DatabaseConfig {
    pub fn acquire_timeout(&self) -> Duration {
        Duration::from_secs(self.acquire_timeout_secs)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    /// HMAC secret used to sign JWTs. MUST be set to a strong random value.
    pub jwt_secret: String,
    /// Access token lifetime in seconds (default 1 hour).
    #[serde(default = "default_access_ttl")]
    pub access_token_ttl_secs: i64,
    /// Refresh token lifetime in seconds (default 30 days).
    #[serde(default = "default_refresh_ttl")]
    pub refresh_token_ttl_secs: i64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            jwt_secret: String::new(),
            access_token_ttl_secs: default_access_ttl(),
            refresh_token_ttl_secs: default_refresh_ttl(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    /// Local filesystem directory for uploaded files.
    #[serde(default = "default_upload_dir")]
    pub upload_dir: String,
    /// Local filesystem directory for database backups.
    #[serde(default = "default_backup_dir")]
    pub backup_dir: String,
    /// Maximum upload size in bytes (default 25 MiB).
    #[serde(default = "default_body_limit")]
    pub max_file_bytes: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            upload_dir: default_upload_dir(),
            backup_dir: default_backup_dir(),
            max_file_bytes: default_body_limit(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct RedisConfig {
    /// Optional Redis URL. When present, cross-instance pub/sub is enabled.
    pub url: Option<String>,
}

impl RedisConfig {
    pub fn enabled(&self) -> bool {
        self.url.is_some()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoggingConfig {
    /// `pretty` or `json`.
    #[serde(default = "default_log_format")]
    pub format: String,
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            format: default_log_format(),
            level: default_log_level(),
        }
    }
}

/// Email delivery and verification policy.
///
/// Transport: if `smtp_host` is set, mail is sent via SMTP; otherwise the
/// engine uses a console transport that logs the action link to the server log.
/// The console default means email flows work out of the box in development with
/// zero configuration (you copy the link from the log), while production sets
/// SMTP. Verification is soft by default: unverified users can still log in
/// (with the app free to nudge them). Set `require_verification = true` to block
/// login until verified. The first user created is always auto-verified so a
/// fresh instance cannot lock itself out before email is configured.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct EmailConfig {
    /// Address used in the From header.
    #[serde(default = "default_from_address")]
    pub from_address: String,
    /// Base URL the app is reached at, used to build links in emails
    /// (e.g. https://chat.example.com). Falls back to a relative link if empty.
    #[serde(default)]
    pub public_base_url: String,
    /// If true, users must verify their email before they can log in.
    #[serde(default)]
    pub require_verification: bool,
    /// How long verification / reset / change tokens remain valid.
    #[serde(default = "default_email_token_ttl_secs")]
    pub token_ttl_secs: u64,
    /// SMTP host. If empty, the console transport is used (links go to the log).
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub smtp_username: String,
    #[serde(default)]
    pub smtp_password: String,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            from_address: default_from_address(),
            public_base_url: String::new(),
            require_verification: false,
            token_ttl_secs: default_email_token_ttl_secs(),
            smtp_host: String::new(),
            smtp_port: default_smtp_port(),
            smtp_username: String::new(),
            smtp_password: String::new(),
        }
    }
}

impl EmailConfig {
    /// True when SMTP is configured; otherwise the console transport is used.
    pub fn smtp_enabled(&self) -> bool {
        !self.smtp_host.trim().is_empty()
    }
}

fn default_from_address() -> String {
    "no-reply@rsmc.local".to_string()
}
fn default_email_token_ttl_secs() -> u64 {
    24 * 60 * 60
}
fn default_smtp_port() -> u16 {
    587
}

/// Rate-limiting policy. Two token buckets protect different surfaces:
/// a stricter per-IP limit on auth endpoints (login/signup/refresh) against
/// brute force, and a looser per-user limit on write endpoints against floods.
/// Disabled with `enabled = false`. In-memory by default (per-instance);
/// sufficient for single-instance deployments, which is the common case.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RateLimitConfig {
    /// Master switch. When false, no limiting is applied.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Max auth attempts per IP within the window (login/signup/refresh).
    #[serde(default = "default_auth_max")]
    pub auth_max: u32,
    /// Auth window length, in seconds.
    #[serde(default = "default_auth_window_secs")]
    pub auth_window_secs: u64,
    /// Max write actions per user within the window (send, upload, etc.).
    #[serde(default = "default_write_max")]
    pub write_max: u32,
    /// Write window length, in seconds.
    #[serde(default = "default_write_window_secs")]
    pub write_window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            enabled: default_true(),
            auth_max: default_auth_max(),
            auth_window_secs: default_auth_window_secs(),
            write_max: default_write_max(),
            write_window_secs: default_write_window_secs(),
        }
    }
}

fn default_auth_max() -> u32 {
    10
}
fn default_auth_window_secs() -> u64 {
    60
}
fn default_write_max() -> u32 {
    30
}
fn default_write_window_secs() -> u64 {
    10
}

/// Message retention: when enabled with a positive `days`, a background sweep
/// soft-deletes messages older than that many days. 0 (the default) disables
/// retention entirely, messages are kept forever. Admin-configurable per
/// instance via config or APP__RETENTION__* env vars.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct RetentionConfig {
    /// Age threshold in days. 0 disables retention.
    #[serde(default)]
    pub days: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self { days: 0 }
    }
}

/// GDPR / account-deletion policy, admin-configurable per instance.
///
/// - "anonymize" (default): the user row is scrubbed to a tombstone and their
///   messages are kept but attributed to a deleted placeholder, so threads and
///   history stay coherent.
/// - "hard_delete": the user's messages and uploads are removed, then the user
///   row is deleted outright.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct GdprConfig {
    pub deletion_mode: DeletionMode,
}

impl Default for GdprConfig {
    fn default() -> Self {
        Self {
            deletion_mode: DeletionMode::Anonymize,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionMode {
    Anonymize,
    HardDelete,
}

impl Settings {
    /// Load config. Precedence: `config.toml` → `APP__*` env → bare env vars.
    pub fn load() -> anyhow::Result<Self> {
        let _ = dotenvy::dotenv();

        let mut builder = config::Config::builder()
            .add_source(config::File::with_name("config").required(false))
            .add_source(
                // No global try_parsing/list_separator: in config 0.14 they
                // turn every string value into a sequence. cors_origins is
                // bridged explicitly below instead.
                config::Environment::with_prefix("APP").separator("__"),
            );

        // Bridge common bare env vars into the structured config.
        if let Ok(v) = std::env::var("DATABASE_URL") {
            builder = builder.set_override("database.url", v)?;
        }
        if let Ok(v) = std::env::var("JWT_SECRET") {
            builder = builder.set_override("auth.jwt_secret", v)?;
        }
        if let Ok(v) = std::env::var("REDIS_URL") {
            builder = builder.set_override("redis.url", v)?;
        }
        if let Ok(v) = std::env::var("PORT") {
            if let Ok(p) = v.parse::<i64>() {
                builder = builder.set_override("server.port", p)?;
            }
        }

        // CORS origins: comma-separated list from CORS_ORIGINS or
        // APP__SERVER__CORS_ORIGINS, split into a real sequence here.
        let cors_raw = std::env::var("CORS_ORIGINS")
            .ok()
            .or_else(|| std::env::var("APP__SERVER__CORS_ORIGINS").ok());
        if let Some(raw) = cors_raw {
            let list: Vec<String> = raw
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !list.is_empty() {
                builder = builder.set_override("server.cors_origins", list)?;
            }
        }

        let settings: Settings = builder.build()?.try_deserialize()?;
        settings.validate()?;
        Ok(settings)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.database.url.is_empty() {
            anyhow::bail!(
                "DATABASE_URL is not set.\n\
                 rsmc-engine needs a PostgreSQL 16+ database. Quick options:\n  \
                 - Docker:   docker run -d -e POSTGRES_PASSWORD=pw -p 5432:5432 postgres:16\n  \
                 - Existing: export DATABASE_URL=postgres://user:pw@host:5432/dbname\n\
                 See docs/NO_DOCKER.md and docs/DEPLOY.md."
            );
        }
        if self.auth.jwt_secret.is_empty() {
            anyhow::bail!(
                "JWT_SECRET is not set.\n\
                 Generate one and set it, for example:\n  \
                 export JWT_SECRET=$(openssl rand -hex 32)"
            );
        }
        if self.auth.jwt_secret.len() < 16 {
            anyhow::bail!(
                "JWT_SECRET is too short ({} chars); use at least 16 (32+ recommended).\n  \
                 export JWT_SECRET=$(openssl rand -hex 32)",
                self.auth.jwt_secret.len()
            );
        }
        Ok(())
    }

    pub fn bind_addr(&self) -> String {
        format!("{}:{}", self.server.host, self.server.port)
    }
}

// ---- Defaults -------------------------------------------------------------

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    8080
}
fn default_cors() -> Vec<String> {
    vec!["*".to_string()]
}
fn default_body_limit() -> usize {
    25 * 1024 * 1024
}
fn default_max_conns() -> u32 {
    20
}
fn default_min_conns() -> u32 {
    2
}
fn default_acquire_timeout_secs() -> u64 {
    10
}
fn default_true() -> bool {
    true
}
fn default_access_ttl() -> i64 {
    3600
}
fn default_refresh_ttl() -> i64 {
    30 * 24 * 3600
}
fn default_upload_dir() -> String {
    "./uploads".to_string()
}
fn default_backup_dir() -> String {
    "./backups".to_string()
}
fn default_log_format() -> String {
    "pretty".to_string()
}
fn default_log_level() -> String {
    "info,rsmc_engine=debug,sqlx=warn".to_string()
}
