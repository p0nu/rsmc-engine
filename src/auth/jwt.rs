//! JWT issuance and verification.

use crate::error::{AppError, AppResult};
use crate::models::user::UserRole;
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The kind of token, embedded as `tok` so an access token can never be used
/// as a refresh token or vice-versa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    Access,
    Refresh,
}

/// JWT claims. `sub` is the user id.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub role: UserRole,
    pub tok: TokenKind,
    /// Unique token id (jti) — used to revoke refresh tokens.
    pub jti: Uuid,
    pub exp: i64,
    pub iat: i64,
}

/// Stateless encoder/decoder holding the signing keys.
#[derive(Clone)]
pub struct JwtKeys {
    encoding: EncodingKey,
    decoding: DecodingKey,
    access_ttl: i64,
    refresh_ttl: i64,
}

/// A freshly minted access + refresh token pair.
pub struct TokenPair {
    pub access: String,
    pub refresh: String,
    pub refresh_jti: Uuid,
    pub access_expires_in: i64,
    pub refresh_expires_at: chrono::DateTime<Utc>,
}

impl JwtKeys {
    pub fn new(secret: &str, access_ttl: i64, refresh_ttl: i64) -> Self {
        Self {
            encoding: EncodingKey::from_secret(secret.as_bytes()),
            decoding: DecodingKey::from_secret(secret.as_bytes()),
            access_ttl,
            refresh_ttl,
        }
    }

    pub fn access_ttl(&self) -> i64 {
        self.access_ttl
    }

    /// Issue a new access + refresh pair for a user.
    pub fn issue_pair(&self, user_id: Uuid, role: UserRole) -> AppResult<TokenPair> {
        let access = self.issue(user_id, role, TokenKind::Access, self.access_ttl)?;
        let refresh_jti = Uuid::new_v4();
        let refresh = self.issue_with_jti(
            user_id,
            role,
            TokenKind::Refresh,
            self.refresh_ttl,
            refresh_jti,
        )?;
        Ok(TokenPair {
            access,
            refresh,
            refresh_jti,
            access_expires_in: self.access_ttl,
            refresh_expires_at: Utc::now() + Duration::seconds(self.refresh_ttl),
        })
    }

    fn issue(&self, sub: Uuid, role: UserRole, tok: TokenKind, ttl: i64) -> AppResult<String> {
        self.issue_with_jti(sub, role, tok, ttl, Uuid::new_v4())
    }

    fn issue_with_jti(
        &self,
        sub: Uuid,
        role: UserRole,
        tok: TokenKind,
        ttl: i64,
        jti: Uuid,
    ) -> AppResult<String> {
        let now = Utc::now();
        let claims = Claims {
            sub,
            role,
            tok,
            jti,
            iat: now.timestamp(),
            exp: (now + Duration::seconds(ttl)).timestamp(),
        };
        Ok(encode(&Header::default(), &claims, &self.encoding)?)
    }

    /// Decode and validate a token, asserting its `TokenKind`.
    pub fn verify(&self, token: &str, expected: TokenKind) -> AppResult<Claims> {
        let data = decode::<Claims>(token, &self.decoding, &Validation::default())?;
        if data.claims.tok != expected {
            return Err(AppError::Unauthorized);
        }
        Ok(data.claims)
    }
}
