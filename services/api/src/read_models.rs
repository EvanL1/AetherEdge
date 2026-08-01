//! Persistence and application read models for the API composition root.
//!
//! These types deliberately carry no HTTP or OpenAPI concerns.

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoleRecord {
    pub id: i64,
    pub name_en: String,
    pub name_zh: String,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

/// The credential columns the auth routes actually read.
///
/// Profile reads go through [`UserProfile`], which joins the role, so this
/// row deliberately does not mirror the whole `users` table.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct UserRecord {
    /// Read by the registration and deactivation tests, not by a route.
    #[cfg_attr(not(test), allow(dead_code))]
    pub id: i64,
    pub password_hash: String,
    /// Read by the role-assignment tests, not by a route.
    #[cfg_attr(not(test), allow(dead_code))]
    pub role_id: i64,
}

#[derive(Debug, Clone)]
pub struct UserProfile {
    pub id: i64,
    pub username: String,
    pub is_active: bool,
    pub last_login: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub role: RoleProfile,
}

#[derive(Debug, Clone)]
pub struct RoleProfile {
    pub id: i64,
    pub name_en: String,
    pub name_zh: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RefreshTokenInfo {
    pub user_id: i64,
    pub expires_at: i64,
}

pub struct IssuedTokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CalculatedPointRecord {
    pub id: i64,
    pub name: String,
    pub formula: Option<String>,
    pub unit: Option<String>,
    pub imgurl: Option<String>,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}
