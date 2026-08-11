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
    //! Minimal SMTP submission with a real STARTTLS upgrade.
    //!
    //! Flow: connect in plaintext, EHLO, issue STARTTLS, then perform an actual
    //! TLS handshake with tokio-rustls and carry out AUTH + the mail transaction
    //! over the encrypted stream. Credentials and message data are never sent on
    //! the plaintext socket. Uses Mozilla's webpki roots for server-cert
    //! verification. For servers that speak implicit TLS on port 465 or that do
    //! not offer STARTTLS, front RSMC with a submission relay.
    use super::{Mail, SmtpTransport};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_rustls::rustls::{self, pki_types::ServerName};
    use tokio_rustls::TlsConnector;

    async fn read_reply(stream: &mut (impl AsyncReadExt + Unpin)) -> anyhow::Result<String> {
        let mut buf = vec![0u8; 2048];
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            anyhow::bail!("SMTP connection closed unexpectedly");
        }
        Ok(String::from_utf8_lossy(&buf[..n]).to_string())
    }

    /// Send a command line and return the server's reply.
    async fn cmd(
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        line: &str,
    ) -> anyhow::Result<String> {
        stream.write_all(line.as_bytes()).await?;
        stream.write_all(b"\r\n").await?;
        read_reply(stream).await
    }

    /// Assert that a reply begins with the expected 3-digit code, else error.
    fn expect(reply: &str, code: &str, step: &str) -> anyhow::Result<()> {
        if reply.starts_with(code) {
            Ok(())
        } else {
            anyhow::bail!("SMTP {step} failed: expected {code}, got {}", reply.trim())
        }
    }

    /// Build a rustls client config trusting the Mozilla root set.
    ///
    /// Uses the ring provider explicitly rather than the process-default
    /// provider: RSMC never calls CryptoProvider::install_default, so relying on
    /// the default would risk a runtime panic ("no process-level CryptoProvider
    /// available"). Being explicit keeps SMTP self-contained.
    fn tls_config() -> anyhow::Result<Arc<rustls::ClientConfig>> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| anyhow::anyhow!("rustls provider setup failed: {e}"))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Arc::new(config))
    }

    /// Run the AUTH + mail transaction over any (post-TLS) stream.
    async fn transact(
        stream: &mut (impl AsyncReadExt + AsyncWriteExt + Unpin),
        t: &SmtpTransport,
        mail: &Mail,
    ) -> anyhow::Result<()> {
        // Re-EHLO over the encrypted channel (required by the protocol after
        // STARTTLS; the server re-advertises capabilities).
        let ehlo = cmd(stream, "EHLO rsmc").await?;
        expect(&ehlo, "250", "EHLO(tls)")?;

        // AUTH LOGIN when credentials are configured.
        if !t.username.is_empty() {
            let a = cmd(stream, "AUTH LOGIN").await?;
            expect(&a, "334", "AUTH LOGIN")?;
            let u = cmd(stream, &base64_encode(t.username.as_bytes())).await?;
            expect(&u, "334", "AUTH username")?;
            let p = cmd(stream, &base64_encode(t.password.as_bytes())).await?;
            expect(&p, "235", "AUTH password")?;
        }

        let mf = cmd(stream, &format!("MAIL FROM:<{}>", t.from)).await?;
        expect(&mf, "250", "MAIL FROM")?;
        let rc = cmd(stream, &format!("RCPT TO:<{}>", mail.to)).await?;
        // 250 or 251 are both acceptable for RCPT.
        if !(rc.starts_with("250") || rc.starts_with("251")) {
            anyhow::bail!("SMTP RCPT TO failed: {}", rc.trim());
        }
        let d = cmd(stream, "DATA").await?;
        expect(&d, "354", "DATA")?;

        // Body, dot-stuffed terminator on its own line.
        let msg = format!(
            "From: {}\r\nTo: {}\r\nSubject: {}\r\nMIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\r\n{}\r\n.",
            t.from, mail.to, mail.subject, mail.body
        );
        let resp = cmd(stream, &msg).await?;
        expect(&resp, "250", "message body")?;

        let _ = cmd(stream, "QUIT").await;
        Ok(())
    }

    pub async fn send(t: &SmtpTransport, mail: Mail) -> anyhow::Result<()> {
        // 1. Plaintext connect + greeting + EHLO.
        let mut plain = TcpStream::connect((t.host.as_str(), t.port)).await?;
        let greeting = read_reply(&mut plain).await?;
        expect(&greeting, "220", "greeting")?;
        let ehlo = cmd(&mut plain, "EHLO rsmc").await?;
        expect(&ehlo, "250", "EHLO")?;

        // 2. STARTTLS: the server must agree with 220 before we hand over any
        //    credentials. If it will not, we refuse rather than fall back to
        //    plaintext auth (that was the old bug).
        let starttls = cmd(&mut plain, "STARTTLS").await?;
        expect(&starttls, "220", "STARTTLS")?;

        // 3. Real TLS handshake over the same socket.
        let connector = TlsConnector::from(tls_config()?);
        let server_name = ServerName::try_from(t.host.clone())
            .map_err(|_| anyhow::anyhow!("invalid SMTP host name for TLS: {}", t.host))?;
        let mut tls = connector.connect(server_name, plain).await?;

        // 4. AUTH + mail transaction, entirely over TLS.
        transact(&mut tls, t, &mail).await
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

    /// Test-only accessor for the base64 helper.
    #[cfg(test)]
    pub(super) fn b64_for_test(input: &[u8]) -> String {
        base64_encode(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 test vectors.
        assert_eq!(smtp::b64_for_test(b""), "");
        assert_eq!(smtp::b64_for_test(b"f"), "Zg==");
        assert_eq!(smtp::b64_for_test(b"fo"), "Zm8=");
        assert_eq!(smtp::b64_for_test(b"foo"), "Zm9v");
        assert_eq!(smtp::b64_for_test(b"foob"), "Zm9vYg==");
        assert_eq!(smtp::b64_for_test(b"fooba"), "Zm9vYmE=");
        assert_eq!(smtp::b64_for_test(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn transport_selection_follows_config() {
        let mut cfg = EmailConfig::default();
        // No SMTP host -> console transport.
        cfg.smtp_host = String::new();
        assert!(!cfg.smtp_enabled());
        // With a host -> SMTP enabled.
        cfg.smtp_host = "smtp.example.com".into();
        assert!(cfg.smtp_enabled());
    }

    #[test]
    fn action_link_composition() {
        let mut cfg = EmailConfig::default();
        cfg.public_base_url = "https://chat.example.com/".into();
        let link = action_link(&cfg, "/verify-email", "abc123");
        assert_eq!(link, "https://chat.example.com/verify-email?token=abc123");
        // Empty base falls back to a relative link.
        cfg.public_base_url = String::new();
        let rel = action_link(&cfg, "/verify-email", "abc123");
        assert_eq!(rel, "/verify-email?token=abc123");
    }
}
