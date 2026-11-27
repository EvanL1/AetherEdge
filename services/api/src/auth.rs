use anyhow::{Result, anyhow};
use chrono::Utc;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::read_models::{IssuedTokenPair, RefreshTokenInfo, UserProfile};

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

impl Claims {
    /// Whether the gateway may forward a mutation to a service that authorizes
    /// commands itself. Only a permission such a service enforces counts.
    #[must_use]
    pub fn may_command_services(&self) -> bool {
        aether_auth_jwt::SERVICE_COMMAND_PERMISSIONS
            .into_iter()
            .any(|permission| self.holds(permission))
    }

    #[must_use]
    pub fn may_operate_gateway(&self) -> bool {
        self.holds(aether_auth_jwt::GATEWAY_OPERATE)
    }

    #[must_use]
    pub fn may_administer(&self) -> bool {
        self.holds(aether_auth_jwt::GATEWAY_ADMINISTER)
    }

    /// The role sets the ceiling and the scope selects within it.
    fn holds(&self, permission: &str) -> bool {
        aether_auth_jwt::permissions_for_role(self.role.as_deref()).contains(&permission)
            && aether_auth_jwt::scope_allows(&self.scope, permission)
    }
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

#[cfg(test)]
mod authority_tests {
    use super::Claims;

    fn claims(role: Option<&str>, scope: &[&str]) -> Claims {
        Claims {
            user_id: 7,
            username: "test-user".to_owned(),
            role: role.map(str::to_owned),
            token_id: None,
            scope: scope.iter().map(|entry| (*entry).to_owned()).collect(),
            exp: 4_102_444_800,
            iat: 1,
            token_type: "access".to_owned(),
        }
    }

    fn full_scope(role: &str) -> Vec<&'static str> {
        aether_auth_jwt::permissions_for_role(Some(role))
    }

    fn other_command_permissions() -> impl Iterator<Item = &'static str> {
        aether_auth_jwt::SERVICE_COMMAND_PERMISSIONS
            .into_iter()
            .chain(aether_auth_jwt::GATEWAY_COMMAND_PERMISSIONS)
    }

    #[test]
    fn a_login_token_keeps_the_authority_of_its_role() {
        for role in ["Engineer", "Admin"] {
            let login = claims(Some(role), &full_scope(role));
            assert!(login.may_operate_gateway(), "{role}");
            assert!(login.may_command_services(), "{role}");
        }
        assert!(claims(Some("Admin"), &full_scope("Admin")).may_administer());
        assert!(!claims(Some("Engineer"), &full_scope("Engineer")).may_administer());

        let viewer = claims(Some("Viewer"), &full_scope("Viewer"));
        assert!(!viewer.may_operate_gateway());
        assert!(!viewer.may_command_services());
        assert!(!viewer.may_administer());
    }

    #[test]
    fn a_missing_or_unknown_role_holds_no_authority_whatever_its_scope() {
        for role in [None, Some("admin"), Some("Owner")] {
            let forged = claims(role, &full_scope("Admin"));
            assert!(!forged.may_operate_gateway(), "{role:?}");
            assert!(!forged.may_command_services(), "{role:?}");
            assert!(!forged.may_administer(), "{role:?}");
        }
    }

    #[test]
    fn one_command_permission_does_not_open_the_gateway_own_routes() {
        for permission in other_command_permissions().chain(["data_processing.run"]) {
            let narrowed = claims(Some("Admin"), &[permission]);
            assert!(!narrowed.may_administer(), "{permission}");
            assert!(!narrowed.may_operate_gateway(), "{permission}");
        }
        assert!(!claims(Some("Admin"), &[]).may_administer());
    }

    #[test]
    fn each_gateway_permission_opens_only_its_own_routes() {
        let operate = claims(Some("Admin"), &["gateway.operate"]);
        assert!(operate.may_operate_gateway());
        assert!(!operate.may_administer());
        assert!(!operate.may_command_services());

        let administer = claims(Some("Admin"), &["gateway.administer"]);
        assert!(administer.may_administer());
        assert!(!administer.may_operate_gateway());
        assert!(!administer.may_command_services());

        let claimed = claims(Some("Engineer"), &["gateway.administer"]);
        assert!(!claimed.may_administer());
    }

    #[test]
    fn only_a_permission_a_service_enforces_passes_the_service_gate() {
        for permission in aether_auth_jwt::SERVICE_COMMAND_PERMISSIONS {
            assert!(
                claims(Some("Engineer"), &[permission]).may_command_services(),
                "{permission}"
            );
        }
        for permission in aether_auth_jwt::GATEWAY_COMMAND_PERMISSIONS
            .into_iter()
            .chain([
                "gateway.operate",
                "gateway.administer",
                "data_processing.read",
            ])
        {
            assert!(
                !claims(Some("Admin"), &[permission]).may_command_services(),
                "{permission}"
            );
        }
    }
}
