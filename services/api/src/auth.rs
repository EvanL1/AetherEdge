use anyhow::{Result, anyhow};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::models::{RefreshTokenInfo, TokenResponse, UserWithRole};

// ── JWT Claims ────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub user_id: i64,
    pub username: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_id: Option<String>,
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

// ── Token Creation ────────────────────────────────────────────────────────────

pub fn create_access_token(
    user: &UserWithRole,
    secret: &str,
    expire_minutes: i64,
) -> Result<String> {
    let now = Utc::now().timestamp() as usize;
    let exp = (Utc::now().timestamp() + expire_minutes * 60) as usize;

    let claims = Claims {
        user_id: user.id,
        username: user.username.clone(),
        role: Some(user.role.name_en.clone()),
        token_id: None,
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
    user: &UserWithRole,
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
        username: user.username.clone(),
        expires_at: now + expire_days * 86400,
    };

    Ok((token, token_id, info))
}

pub fn create_token_pair(
    user: &UserWithRole,
    secret: &str,
    access_expire_minutes: i64,
    refresh_expire_days: i64,
) -> Result<(TokenResponse, String, RefreshTokenInfo)> {
    let access_token = create_access_token(user, secret, access_expire_minutes)?;
    let (refresh_token, token_id, token_info) =
        create_refresh_token(user, secret, refresh_expire_days)?;

    let response = TokenResponse {
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
