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
    /// Maximum upload size in bytes (default 25 MiB).
    #[serde(default = "default_body_limit")]
    pub max_file_bytes: usize,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            upload_dir: default_upload_dir(),
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
        if self.auth.jwt_secret.len() < 16 {
            anyhow::bail!("auth.jwt_secret must be at least 16 characters");
        }
        if self.database.url.is_empty() {
            anyhow::bail!("database.url (DATABASE_URL) must be set");
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
fn default_log_format() -> String {
    "pretty".to_string()
}
fn default_log_level() -> String {
    "info,rsmc_engine=debug,sqlx=warn".to_string()
}
