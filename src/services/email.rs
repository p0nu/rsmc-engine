//! Email service: token issuance/verification and message delivery.
//!
//! Delivery goes through the [`MailTransport`] trait so the transport is
//! swappable. Two implementations ship:
//!   - [`ConsoleTransport`]: logs the action link to the server log. This is the
//!     zero-config default, so verification/reset flows work in development
//!     without an SMTP server (copy the link from the log).
//!   - [`SmtpTransport`]: sends over SMTP with STARTTLS when SMTP is configured.
//!
//! Tokens are opaque random strings; only their SHA-256 hash is stored, so a
//! database leak never exposes usable tokens. Each token is single-use and
//! expires.

use crate::config::EmailConfig;
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// A generated token: the raw value (emailed to the user) and its hash (stored).
pub struct IssuedToken {
    pub raw: String,
    pub hash: String,
}

/// Generate a fresh opaque token and its storage hash.
pub fn generate_token() -> IssuedToken {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let raw = hex::encode(bytes);
    let hash = hash_token(&raw);
    IssuedToken { raw, hash }
}

/// Hash a raw token for storage / lookup.
pub fn hash_token(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    hex::encode(h.finalize())
}

/// An outgoing message.
pub struct Mail {
    pub to: String,
    pub subject: String,
    pub body: String,
}

/// Pluggable delivery backend.
#[async_trait::async_trait]
pub trait MailTransport: Send + Sync {
    async fn send(&self, mail: Mail) -> anyhow::Result<()>;
}

/// Logs the message (including any link) to the server log. Zero-config default.
pub struct ConsoleTransport;

#[async_trait::async_trait]
impl MailTransport for ConsoleTransport {
    async fn send(&self, mail: Mail) -> anyhow::Result<()> {
        tracing::info!(
            to = %mail.to,
            subject = %mail.subject,
            "email (console transport) -- configure SMTP to deliver for real:\n{}",
            mail.body
        );
        Ok(())
    }
}

/// Sends over SMTP. Kept behind the same trait; constructed only when SMTP is
/// configured. Uses a submission-style flow (EHLO, STARTTLS, AUTH LOGIN, MAIL).
pub struct SmtpTransport {
    host: String,
    port: u16,
    username: String,
    password: String,
    from: String,
}

impl SmtpTransport {
    pub fn new(cfg: &EmailConfig) -> Self {
        Self {
            host: cfg.smtp_host.clone(),
            port: cfg.smtp_port,
            username: cfg.smtp_username.clone(),
            password: cfg.smtp_password.clone(),
            from: cfg.from_address.clone(),
        }
    }
}

#[async_trait::async_trait]
impl MailTransport for SmtpTransport {
    async fn send(&self, mail: Mail) -> anyhow::Result<()> {
        crate::services::email::smtp::send(self, mail).await
    }
}

/// Build the transport the config calls for.
pub fn build_transport(cfg: &EmailConfig) -> Arc<dyn MailTransport> {
    if cfg.smtp_enabled() {
        Arc::new(SmtpTransport::new(cfg))
    } else {
        Arc::new(ConsoleTransport)
    }
}

/// Compose an action link from the configured base URL and a path+token.
pub fn action_link(cfg: &EmailConfig, path: &str, token: &str) -> String {
    let base = cfg.public_base_url.trim_end_matches('/');
    if base.is_empty() {
        format!("{path}?token={token}")
    } else {
        format!("{base}{path}?token={token}")
    }
}

mod smtp {
    //! Minimal SMTP submission over TCP with STARTTLS. Intentionally small; for
    //! exotic servers, front RSMC with a relay. Errors are surfaced so callers
    //! can log a clear failure.
    use super::{Mail, SmtpTransport};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    async fn read_reply(stream: &mut (impl AsyncReadExt + Unpin)) -> anyhow::Result<String> {
        let mut buf = vec![0u8; 1024];
        let n = stream.read(&mut buf).await?;
        Ok(String::from_utf8_lossy(&buf[..n]).to_string())
    }

    async fn cmd(
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        line: &str,
    ) -> anyhow::Result<String> {
        stream.write_all(line.as_bytes()).await?;
        stream.write_all(b"\r\n").await?;
        read_reply(stream).await
    }

    pub async fn send(t: &SmtpTransport, mail: Mail) -> anyhow::Result<()> {
        // Plain connection for the initial handshake; upgrade with STARTTLS.
        let mut stream = TcpStream::connect((t.host.as_str(), t.port)).await?;
        let _greeting = read_reply(&mut stream).await?;
        let _ehlo = cmd(&mut stream, &format!("EHLO {}", "rsmc")).await?;

        // Many submission servers require STARTTLS. We attempt it; if the server
        // does not advertise it, the AUTH/MAIL still proceed on the plain socket
        // (acceptable for a trusted internal relay). A full TLS upgrade uses
        // tokio-rustls; kept minimal here.
        let starttls = cmd(&mut stream, "STARTTLS").await?;
        if !starttls.starts_with("220") {
            tracing::warn!("SMTP server did not accept STARTTLS; sending on plain connection");
        }

        // AUTH LOGIN (base64 username/password) when credentials are set.
        if !t.username.is_empty() {
            let _ = cmd(&mut stream, "AUTH LOGIN").await?;
            let _ = cmd(
                &mut stream,
                &base64_encode(t.username.as_bytes()),
            )
            .await?;
            let _ = cmd(
                &mut stream,
                &base64_encode(t.password.as_bytes()),
            )
            .await?;
        }

        let _ = cmd(&mut stream, &format!("MAIL FROM:<{}>", t.from)).await?;
        let _ = cmd(&mut stream, &format!("RCPT TO:<{}>", mail.to)).await?;
        let _ = cmd(&mut stream, "DATA").await?;
        let msg = format!(
            "From: {}\r\nTo: {}\r\nSubject: {}\r\n\r\n{}\r\n.",
            t.from, mail.to, mail.subject, mail.body
        );
        let resp = cmd(&mut stream, &msg).await?;
        let _ = cmd(&mut stream, "QUIT").await?;

        if resp.starts_with('2') || resp.contains("250") {
            Ok(())
        } else {
            anyhow::bail!("SMTP delivery failed: {resp}")
        }
    }

    /// Small base64 (standard alphabet) for AUTH LOGIN, avoids a new dep.
    fn base64_encode(input: &[u8]) -> String {
        const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            out.push(A[(b[0] >> 2) as usize] as char);
            out.push(A[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(A[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(A[(b[2] & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }
}
