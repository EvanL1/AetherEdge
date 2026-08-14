use anyhow::{Result, anyhow};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::read_models::{IssuedTokenPair, RefreshTokenInfo, UserProfile};

const PASSWORD_TASK_CONCURRENCY: usize = 4;
static PASSWORD_TASK_SLOTS: Semaphore = Semaphore::const_new(PASSWORD_TASK_CONCURRENCY);

/// Fixed hash used to make an unknown-account login consume the same bcrypt
/// work as a known account. It is not associated with a real credential.
pub const DUMMY_PASSWORD_HASH: &str =
    "$2y$12$OltE7DhWX/gbFlqOEgpnzOYbuOrFslyCdU4ZM8c3zgBuxb0eyRQxq";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordTaskError {
    Busy,
    Failed,
}

// ── JWT Claims ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub user_id: i64,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
    /// The permissions this token may exercise.
    ///
    /// Always written and always read. A token that does not state its
    /// permissions is malformed rather than fully privileged.
    pub scope: Vec<String>,
    pub exp: usize,
    pub iat: usize,
    #[serde(rename = "type")]
    pub token_type: String,
}

// ── Password Hashing ──────────────────────────────────────────────────────────

/// Hash a password (frontend sends MD5, we bcrypt the MD5 for storage).
pub fn hash_password(md5_password: &str) -> Result<String> {
    bcrypt::hash(md5_password, bcrypt::DEFAULT_COST)
        .map_err(|e| anyhow!("bcrypt hash failed: {}", e))
}

/// Verify a password against a stored bcrypt hash.
pub fn verify_password(md5_password: &str, hash: &str) -> bool {
    bcrypt::verify(md5_password, hash).unwrap_or(false)
}

/// Hashes a password on the blocking pool while enforcing a hard process-wide
/// concurrency limit. Requests beyond the limit fail fast instead of queuing
/// unbounded CPU work on the API runtime.
pub async fn hash_password_async(md5_password: &str) -> Result<String, PasswordTaskError> {
    let permit = PASSWORD_TASK_SLOTS
        .try_acquire()
        .map_err(|_| PasswordTaskError::Busy)?;
    let password = md5_password.to_owned();
    tokio::task::spawn_blocking(move || {
        // Keep the permit inside the blocking job. If the HTTP request is
        // cancelled, bcrypt keeps running and must still count against the
        // process-wide CPU limit until it actually finishes.
        let _permit = permit;
        hash_password(&password)
    })
    .await
    .map_err(|_| PasswordTaskError::Failed)?
    .map_err(|_| PasswordTaskError::Failed)
}

/// Verifies a password on the blocking pool with the same bounded concurrency
/// policy as hashing.
pub async fn verify_password_async(
    md5_password: &str,
    hash: &str,
) -> Result<bool, PasswordTaskError> {
    let permit = PASSWORD_TASK_SLOTS
        .try_acquire()
        .map_err(|_| PasswordTaskError::Busy)?;
    let password = md5_password.to_owned();
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || {
        // See `hash_password_async`: cancellation must not release capacity
        // while the detached blocking job is still consuming CPU.
        let _permit = permit;
        verify_password(&password, &hash)
    })
    .await
    .map_err(|_| PasswordTaskError::Failed)
}

// ── Token Creation ────────────────────────────────────────────────────────────

pub fn create_access_token(
    user: &UserProfile,
    secret: &str,
    expire_minutes: i64,
) -> Result<String> {
    let now = Utc::now().timestamp() as usize;
    let exp = (Utc::now().timestamp() + expire_minutes * 60) as usize;

    // A login token states the whole set its role may hold. Writing it out
    // rather than leaving it implied means every token answers "what may this
    // credential do" from its own contents.
    let claims = Claims {
        user_id: user.id,
        username: user.username.clone(),
        role: Some(user.role.name_en.clone()),
        token_id: None,
        scope: aether_auth_jwt::permissions_for_role(Some(&user.role.name_en))
            .into_iter()
            .map(str::to_owned)
            .collect(),
        exp,
        iat: now,
        token_type: "access".to_string(),
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| anyhow!("JWT encode failed: {}", e))
}

/// Creates a refresh token and returns (token_string, token_id, token_info).
pub fn create_refresh_token(
    user: &UserProfile,
    secret: &str,
    expire_days: i64,
) -> Result<(String, String, RefreshTokenInfo)> {
    let token_id = Uuid::new_v4().to_string();
    let now = Utc::now().timestamp();
    let exp = (now + expire_days * 86400) as usize;

    let claims = Claims {
        user_id: user.id,
        username: user.username.clone(),
        role: None,
        token_id: Some(token_id.clone()),
        scope: Vec::new(),
        exp,
        iat: now as usize,
        token_type: "refresh".to_string(),
    };

    let token = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| anyhow!("JWT encode failed: {}", e))?;

    let info = RefreshTokenInfo {
        user_id: user.id,
        expires_at: now + expire_days * 86400,
    };

    Ok((token, token_id, info))
}

pub fn create_token_pair(
    user: &UserProfile,
    secret: &str,
    access_expire_minutes: i64,
    refresh_expire_days: i64,
) -> Result<(IssuedTokenPair, String, RefreshTokenInfo)> {
    let access_token = create_access_token(user, secret, access_expire_minutes)?;
    let (refresh_token, token_id, token_info) =
        create_refresh_token(user, secret, refresh_expire_days)?;

    let response = IssuedTokenPair {
        access_token,
        refresh_token,
        token_type: "bearer".to_string(),
        expires_in: access_expire_minutes * 60,
    };

    Ok((response, token_id, token_info))
}

// ── Token Verification ────────────────────────────────────────────────────────

/// Verifies an access token and returns the Claims on success.
pub fn verify_access_token(token: &str, secret: &str) -> Option<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()
    .and_then(|data| {
        if data.claims.token_type == "access" {
            Some(data.claims)
        } else {
            None
        }
    })
}

/// Verifies a refresh token and returns the Claims on success.
pub fn verify_refresh_token(token: &str, secret: &str) -> Option<Claims> {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_exp = true;

    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .ok()
    .and_then(|data| {
        if data.claims.token_type == "refresh" {
            Some(data.claims)
        } else {
            None
        }
    })
}
