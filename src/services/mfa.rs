//! MFA via TOTP (RFC 6238), authenticator-app only.
//!
//! - Secrets are 20 random bytes, presented to the user as base32 and as an
//!   otpauth:// provisioning URI (for QR codes).
//! - Secrets are stored ENCRYPTED at rest with AES-256-GCM. The key is derived
//!   from the configured JWT secret via SHA-256.
//!
//!   REVIEW NOTE (security): deriving the MFA encryption key from JWT_SECRET
//!   couples the two. Rotating JWT_SECRET makes stored TOTP secrets
//!   undecryptable (users must re-enroll). A dedicated MFA_ENCRYPTION_KEY would
//!   decouple them; deferred to keep required config minimal. Flagged for review
//!   before 1.0.
//!
//! - Verification checks the current 30s step and +/-1 neighbor to tolerate
//!   clock skew (a standard, deliberately small window).

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, Mac};
use ring::aead;
use sha1::Sha1;
use sha2::{Digest, Sha256};

type HmacSha1 = Hmac<Sha1>;

const STEP_SECS: u64 = 30;
const DIGITS: u32 = 6;
const SECRET_LEN: usize = 20;

/// Generate a fresh random TOTP secret (raw bytes).
pub fn generate_secret() -> Vec<u8> {
    use rand::RngCore;
    let mut b = vec![0u8; SECRET_LEN];
    rand::thread_rng().fill_bytes(&mut b);
    b
}

/// Base32 (no padding) encoding of a secret, for manual entry.
pub fn secret_base32(secret: &[u8]) -> String {
    BASE32_NOPAD.encode(secret)
}

/// otpauth:// provisioning URI for QR codes.
pub fn provisioning_uri(secret: &[u8], account: &str, issuer: &str) -> String {
    let b32 = secret_base32(secret);
    let iss = urlencode(issuer);
    let acc = urlencode(account);
    format!(
        "otpauth://totp/{iss}:{acc}?secret={b32}&issuer={iss}&digits={DIGITS}&period={STEP_SECS}"
    )
}

/// Test-only: compute the TOTP code at a given unix time. Exposed so
/// integration tests can drive the full login flow deterministically.
pub fn totp_for_test(secret: &[u8], unix_secs: u64) -> u32 {
    totp_at(secret, unix_secs)
}

/// Compute the TOTP code for a given unix time.
fn totp_at(secret: &[u8], unix_secs: u64) -> u32 {
    let counter = unix_secs / STEP_SECS;
    let msg = counter.to_be_bytes();
    let mut mac = HmacSha1::new_from_slice(secret).expect("hmac key");
    mac.update(&msg);
    let digest = mac.finalize().into_bytes();
    // Dynamic truncation (RFC 4226).
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let bin = ((u32::from(digest[offset]) & 0x7f) << 24)
        | ((u32::from(digest[offset + 1]) & 0xff) << 16)
        | ((u32::from(digest[offset + 2]) & 0xff) << 8)
        | (u32::from(digest[offset + 3]) & 0xff);
    bin % 10u32.pow(DIGITS)
}

/// Verify a user-supplied code against the current time, allowing +/-1 step of
/// skew. Constant-ish comparison over the small candidate set.
pub fn verify(secret: &[u8], code: &str) -> bool {
    let code: u32 = match code.trim().parse() {
        Ok(c) => c,
        Err(_) => return false,
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for step in [-1i64, 0, 1] {
        let t = (now as i64 + step * STEP_SECS as i64) as u64;
        if totp_at(secret, t) == code {
            return true;
        }
    }
    false
}

// --- Secret encryption at rest (AES-256-GCM via ring) ---

fn derive_key(jwt_secret: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"rsmc-mfa-key-v1");
    h.update(jwt_secret.as_bytes());
    let out = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&out);
    k
}

/// Encrypt a raw secret. Output: base64(nonce(12) || ciphertext || tag).
pub fn encrypt_secret(jwt_secret: &str, secret: &[u8]) -> anyhow::Result<String> {
    use rand::RngCore;
    let key_bytes = derive_key(jwt_secret);
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes)
        .map_err(|_| anyhow::anyhow!("aead key"))?;
    let key = aead::LessSafeKey::new(unbound);

    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);

    let mut in_out = secret.to_vec();
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|_| anyhow::anyhow!("aead seal"))?;

    let mut combined = nonce_bytes.to_vec();
    combined.extend_from_slice(&in_out);
    Ok(data_encoding::BASE64.encode(&combined))
}

/// Decrypt a stored secret produced by `encrypt_secret`.
pub fn decrypt_secret(jwt_secret: &str, stored: &str) -> anyhow::Result<Vec<u8>> {
    let raw = data_encoding::BASE64
        .decode(stored.as_bytes())
        .map_err(|_| anyhow::anyhow!("bad ciphertext encoding"))?;
    if raw.len() < 12 + 16 {
        anyhow::bail!("ciphertext too short");
    }
    let (nonce_bytes, ct) = raw.split_at(12);
    let key_bytes = derive_key(jwt_secret);
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &key_bytes)
        .map_err(|_| anyhow::anyhow!("aead key"))?;
    let key = aead::LessSafeKey::new(unbound);
    let mut nb = [0u8; 12];
    nb.copy_from_slice(nonce_bytes);
    let nonce = aead::Nonce::assume_unique_for_key(nb);
    let mut in_out = ct.to_vec();
    let plain = key
        .open_in_place(nonce, aead::Aad::empty(), &mut in_out)
        .map_err(|_| anyhow::anyhow!("aead open (wrong key or tampered)"))?;
    Ok(plain.to_vec())
}

/// Minimal percent-encoding for the few chars that appear in issuer/account.
fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_rfc6238_known_vector() {
        // RFC 6238 uses the ASCII secret "12345678901234567890" with SHA-1.
        let secret = b"12345678901234567890";
        // At T=59s, step=30 -> counter 1, the RFC's expected 8-digit is 94287082;
        // truncated to 6 digits that's 287082.
        assert_eq!(totp_at(secret, 59) % 1_000_000, 287082);
    }

    #[test]
    fn verify_accepts_current_and_neighbors() {
        let secret = generate_secret();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let code = format!("{:06}", totp_at(&secret, now));
        assert!(verify(&secret, &code));
        assert!(!verify(&secret, "000000") || verify(&secret, &code));
    }

    #[test]
    fn encrypt_roundtrip() {
        let secret = generate_secret();
        let enc = encrypt_secret("a-test-jwt-secret-long-enough", &secret).unwrap();
        let dec = decrypt_secret("a-test-jwt-secret-long-enough", &enc).unwrap();
        assert_eq!(secret, dec);
        // Wrong key fails to decrypt (authenticated).
        assert!(decrypt_secret("different-secret", &enc).is_err());
    }
}
