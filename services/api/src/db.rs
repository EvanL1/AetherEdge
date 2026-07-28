use anyhow::Result;
use sqlx::{Row, SqlitePool};

use crate::models::{Role, RoleInfo, UserRow, UserWithRole};

// ── Schema ────────────────────────────────────────────────────────────────────

pub async fn create_tables(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS roles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name_en VARCHAR(50) NOT NULL UNIQUE,
            name_zh VARCHAR(50) NOT NULL UNIQUE,
            description TEXT,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            username VARCHAR(50) NOT NULL UNIQUE,
            password_hash VARCHAR(255) NOT NULL,
            role_id INTEGER NOT NULL DEFAULT 3,
            is_active BOOLEAN DEFAULT 1,
            last_login TIMESTAMP NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            FOREIGN KEY (role_id) REFERENCES roles (id)
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_users_username ON users(username)")
        .execute(pool)
        .await?;

    Ok(())
}

pub async fn init_roles(pool: &SqlitePool) -> Result<()> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM roles")
        .fetch_one(pool)
        .await?;

    if count == 0 {
        sqlx::query(
            "INSERT INTO roles (id, name_en, name_zh, description) VALUES
             (1, 'Admin', '管理员', '系统管理员，拥有所有权限'),
             (2, 'Engineer', '工程师', '工程师，可以进行设备操作和配置'),
             (3, 'Viewer', '查看者', '只读用户，只能查看数据')",
        )
        .execute(pool)
        .await?;
    }

    Ok(())
}

// ── User Queries ──────────────────────────────────────────────────────────────

pub async fn get_user_by_username(pool: &SqlitePool, username: &str) -> Result<Option<UserRow>> {
    Ok(
        sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn get_user_by_id(pool: &SqlitePool, user_id: i64) -> Result<Option<UserRow>> {
    Ok(
        sqlx::query_as::<_, UserRow>("SELECT * FROM users WHERE id = ?")
            .bind(user_id)
            .fetch_optional(pool)
            .await?,
    )
}

fn row_to_user_with_role(r: sqlx::sqlite::SqliteRow) -> UserWithRole {
    UserWithRole {
        id: r.get("id"),
        username: r.get("username"),
        is_active: r.get("is_active"),
        last_login: r.get("last_login"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
        role: RoleInfo {
            id: r.get("role_id"),
            name_en: r.get("role_name_en"),
            name_zh: r.get("role_name_zh"),
            description: r.get("role_description"),
        },
    }
}

pub async fn get_user_with_role(pool: &SqlitePool, user_id: i64) -> Result<Option<UserWithRole>> {
    let row = sqlx::query(
        "SELECT u.id, u.username, u.is_active, u.last_login, u.created_at, u.updated_at,
                r.id AS role_id, r.name_en AS role_name_en, r.name_zh AS role_name_zh,
                r.description AS role_description
         FROM users u JOIN roles r ON u.role_id = r.id
         WHERE u.id = ?",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_user_with_role))
}

pub async fn get_user_with_role_by_username(
    pool: &SqlitePool,
    username: &str,
) -> Result<Option<UserWithRole>> {
    let row = sqlx::query(
        "SELECT u.id, u.username, u.is_active, u.last_login, u.created_at, u.updated_at,
                r.id AS role_id, r.name_en AS role_name_en, r.name_zh AS role_name_zh,
                r.description AS role_description
         FROM users u JOIN roles r ON u.role_id = r.id
         WHERE u.username = ?",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(row_to_user_with_role))
}

pub async fn create_user(
    pool: &SqlitePool,
    username: &str,
    password_hash: &str,
    role_id: i64,
) -> Result<i64> {
    let result =
        sqlx::query("INSERT INTO users (username, password_hash, role_id) VALUES (?, ?, ?)")
            .bind(username)
            .bind(password_hash)
            .bind(role_id)
            .execute(pool)
            .await?;

    Ok(result.last_insert_rowid())
}

pub async fn update_user_last_login(pool: &SqlitePool, user_id: i64) -> Result<()> {
    sqlx::query(
        "UPDATE users SET last_login = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
    )
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn update_user_role(pool: &SqlitePool, user_id: i64, role_id: i64) -> Result<()> {
    sqlx::query("UPDATE users SET role_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(role_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_user_active(pool: &SqlitePool, user_id: i64, is_active: bool) -> Result<()> {
    sqlx::query("UPDATE users SET is_active = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(is_active)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn update_user_password(
    pool: &SqlitePool,
    user_id: i64,
    password_hash: &str,
) -> Result<()> {
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?")
        .bind(password_hash)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn delete_user(pool: &SqlitePool, user_id: i64) -> Result<bool> {
    let result = sqlx::query("DELETE FROM users WHERE id = ?")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn get_all_roles(pool: &SqlitePool) -> Result<Vec<Role>> {
    Ok(sqlx::query_as::<_, Role>("SELECT * FROM roles ORDER BY id")
        .fetch_all(pool)
        .await?)
}

pub async fn get_all_users_with_roles(pool: &SqlitePool) -> Result<Vec<UserWithRole>> {
    let rows = sqlx::query(
        "SELECT u.id, u.username, u.is_active, u.last_login, u.created_at, u.updated_at,
                r.id AS role_id, r.name_en AS role_name_en, r.name_zh AS role_name_zh,
                r.description AS role_description
         FROM users u JOIN roles r ON u.role_id = r.id
         ORDER BY u.id",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(row_to_user_with_role).collect())
}
