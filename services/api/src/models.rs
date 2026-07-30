use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ── Role ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Role {
    pub id: i64,
    pub name_en: String,
    pub name_zh: String,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

// ── User ──────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UserWithRole {
    pub id: i64,
    pub username: String,
    pub is_active: bool,
    pub last_login: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub role: RoleInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RoleInfo {
    pub id: i64,
    pub name_en: String,
    pub name_zh: String,
    pub description: Option<String>,
}

// ── Auth DTOs ─────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize, ToSchema)]
#[schema(example = json!({"username": "operator1", "password": "e10adc3949ba59abbe56e057f20f883e"}))]
pub struct UserCreate {
    /// Username
    pub username: String,
    /// MD5-hashed password supplied by the frontend
    pub password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(example = json!({"username": "admin", "password": "e10adc3949ba59abbe56e057f20f883e"}))]
pub struct UserLogin {
    pub username: String,
    /// MD5-hashed password supplied by the frontend
    pub password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UserUpdate {
    pub role_id: Option<i64>,
    pub is_active: Option<bool>,
    pub old_password: Option<String>,
    pub new_password: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(example = json!({"old_password": "e10adc3949ba59abbe56e057f20f883e", "new_password": "<MD5 hash of new password>"}))]
pub struct PasswordChange {
    pub old_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize, ToSchema)]
#[schema(example = json!({"refresh_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9..."}))]
pub struct RefreshTokenRequest {
    pub refresh_token: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: i64,
}

/// Compatibility response envelope used by the gateway auth routes.
///
/// The gateway predates `common::SuccessResponse` and includes a human-readable
/// message alongside typed data. Keep this schema explicit so generated clients
/// match the wire format during migration.
#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, Serialize, ToSchema)]
pub struct GatewayDataResponse<T> {
    pub success: bool,
    pub message: String,
    pub data: T,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, Serialize, ToSchema)]
pub struct GatewayMessageResponse {
    pub success: bool,
    pub message: String,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct RegistrationResult {
    pub id: i64,
    pub username: String,
    pub role_id: i64,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct RoleListResponse {
    pub success: bool,
    pub message: String,
    pub data: Vec<Role>,
    pub total: usize,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct UserListData {
    pub total: usize,
    pub list: Vec<UserWithRole>,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct DeletedUserData {
    pub user_id: i64,
    pub username: String,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct AuthStatsData {
    pub active_refresh_tokens: usize,
    pub expired_tokens: usize,
    pub access_token_expire_minutes: i64,
    pub refresh_token_expire_days: i64,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct HomepagePageData {
    pub items: Vec<CalculatedPoint>,
    pub total: i64,
    pub page: i64,
    pub limit: i64,
    pub pages: i64,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct HomepageResetData {
    /// Number of homepage point definitions after reset; always zero.
    pub remaining_count: i64,
    /// Confirms that reset does not import domain-specific defaults.
    pub note: String,
}

#[allow(dead_code)] // OpenAPI-only compatibility schema.
#[derive(Debug, ToSchema)]
pub struct UserUpdateSuccess {
    pub success: bool,
    pub message: String,
    /// Present for profile updates; omitted for the compatibility password path.
    pub data: Option<UserWithRole>,
}

// ── Calculated Points ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CalculatedPoint {
    pub id: i64,
    pub name: String,
    pub formula: Option<String>,
    pub unit: Option<String>,
    pub imgurl: Option<String>,
    pub description: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CalculatedPointUpdate {
    pub name: Option<String>,
    pub formula: Option<String>,
    pub unit: Option<String>,
    pub imgurl: Option<String>,
    pub description: Option<String>,
}

// ── Network Config ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct NetworkConfig {
    pub dhcp: bool,
    pub ip: String,
    pub subnet_mask: String,
    pub gateway: String,
    pub dns1: String,
    pub dns2: String,
}

impl From<crate::read_models::RoleRecord> for Role {
    fn from(value: crate::read_models::RoleRecord) -> Self {
        Self {
            id: value.id,
            name_en: value.name_en,
            name_zh: value.name_zh,
            description: value.description,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

impl From<crate::read_models::RoleProfile> for RoleInfo {
    fn from(value: crate::read_models::RoleProfile) -> Self {
        Self {
            id: value.id,
            name_en: value.name_en,
            name_zh: value.name_zh,
            description: value.description,
        }
    }
}

impl From<crate::read_models::UserProfile> for UserWithRole {
    fn from(value: crate::read_models::UserProfile) -> Self {
        Self {
            id: value.id,
            username: value.username,
            is_active: value.is_active,
            last_login: value.last_login,
            created_at: value.created_at,
            updated_at: value.updated_at,
            role: value.role.into(),
        }
    }
}

impl From<crate::read_models::IssuedTokenPair> for TokenResponse {
    fn from(value: crate::read_models::IssuedTokenPair) -> Self {
        Self {
            access_token: value.access_token,
            refresh_token: value.refresh_token,
            token_type: value.token_type,
            expires_in: value.expires_in,
        }
    }
}

impl From<crate::read_models::CalculatedPointRecord> for CalculatedPoint {
    fn from(value: crate::read_models::CalculatedPointRecord) -> Self {
        Self {
            id: value.id,
            name: value.name,
            formula: value.formula,
            unit: value.unit,
            imgurl: value.imgurl,
            description: value.description,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}
