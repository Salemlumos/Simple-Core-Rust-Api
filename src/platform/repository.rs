//! Queries do banco `control`. Alterações de superusuário sempre filtram por
//! `role = 'superuser'`: nenhuma query daqui altera o owner.

use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

pub async fn audit(
    conn: &mut PgConnection,
    actor: Uuid,
    action: &str,
    target_type: &str,
    target_id: impl ToString,
    payload: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO platform_audit (id, actor_id, action, target_type, target_id, payload)
         VALUES ($1, $2, $3, $4, $5, $6)",
        Uuid::now_v7(),
        actor,
        action,
        target_type,
        target_id.to_string(),
        payload
    )
    .execute(conn)
    .await
    .map(drop)
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TenantOut {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    /// `provisioning`, `active`, `failed` ou `suspended`.
    pub status: String,
    /// Último erro de provisionamento (quando `failed`).
    pub last_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

pub async fn list_tenants<'e, E: PgExecutor<'e>>(
    exec: E,
    after: Uuid,
    limit: i64,
) -> Result<Vec<TenantOut>, sqlx::Error> {
    sqlx::query_as!(
        TenantOut,
        "SELECT id, slug, name, status, last_error, created_at, updated_at
         FROM tenants WHERE id > $1 ORDER BY id LIMIT $2",
        after,
        limit
    )
    .fetch_all(exec)
    .await
}

pub async fn get_tenant<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> Result<Option<TenantOut>, sqlx::Error> {
    sqlx::query_as!(
        TenantOut,
        "SELECT id, slug, name, status, last_error, created_at, updated_at FROM tenants WHERE id = $1",
        id
    )
    .fetch_optional(exec)
    .await
}

pub async fn insert_tenant(
    conn: &mut PgConnection,
    id: Uuid,
    slug: &str,
    name: &str,
    db_name: &str,
    created_by: Uuid,
) -> Result<TenantOut, sqlx::Error> {
    sqlx::query_as!(
        TenantOut,
        "INSERT INTO tenants (id, slug, name, db_name, status, created_by)
         VALUES ($1, $2, $3, $4, 'provisioning', $5)
         RETURNING id, slug, name, status, last_error, created_at, updated_at",
        id,
        slug,
        name,
        db_name,
        created_by
    )
    .fetch_one(conn)
    .await
}

/// Altera nome e/ou status; o status só transita entre `active` e `suspended`.
pub async fn update_tenant(
    conn: &mut PgConnection,
    id: Uuid,
    name: Option<&str>,
    status: Option<&str>,
) -> Result<Option<TenantOut>, sqlx::Error> {
    sqlx::query_as!(
        TenantOut,
        "UPDATE tenants SET
           name = COALESCE($2, name),
           status = COALESCE($3, status),
           updated_at = now()
         WHERE id = $1 AND ($3::text IS NULL OR status IN ('active', 'suspended'))
         RETURNING id, slug, name, status, last_error, created_at, updated_at",
        id,
        name,
        status
    )
    .fetch_optional(conn)
    .await
}

pub async fn retry_tenant(conn: &mut PgConnection, id: Uuid) -> Result<Option<TenantOut>, sqlx::Error> {
    sqlx::query_as!(
        TenantOut,
        "UPDATE tenants SET status = 'provisioning', last_error = NULL, updated_at = now()
         WHERE id = $1 AND status = 'failed'
         RETURNING id, slug, name, status, last_error, created_at, updated_at",
        id
    )
    .fetch_optional(conn)
    .await
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlatformUserOut {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    /// `owner` ou `superuser`.
    pub role: String,
    pub is_active: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

pub async fn get_platform_user<'e, E: PgExecutor<'e>>(
    exec: E,
    id: Uuid,
) -> Result<Option<PlatformUserOut>, sqlx::Error> {
    sqlx::query_as!(
        PlatformUserOut,
        "SELECT id, email, name, role, is_active, created_at, updated_at FROM platform_users WHERE id = $1",
        id
    )
    .fetch_optional(exec)
    .await
}

pub async fn platform_password_hash<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!("SELECT password_hash FROM platform_users WHERE id = $1", id)
        .fetch_optional(exec)
        .await
}

/// O próprio usuário de plataforma: nome e senha.
pub async fn update_me(
    conn: &mut PgConnection,
    id: Uuid,
    name: Option<&str>,
    password_hash: Option<&str>,
) -> Result<Option<PlatformUserOut>, sqlx::Error> {
    sqlx::query_as!(
        PlatformUserOut,
        "UPDATE platform_users SET
           name = COALESCE($2, name),
           password_hash = COALESCE($3, password_hash),
           updated_at = now()
         WHERE id = $1
         RETURNING id, email, name, role, is_active, created_at, updated_at",
        id,
        name,
        password_hash
    )
    .fetch_optional(conn)
    .await
}

pub async fn list_superusers<'e, E: PgExecutor<'e>>(
    exec: E,
    after: Uuid,
    limit: i64,
) -> Result<Vec<PlatformUserOut>, sqlx::Error> {
    sqlx::query_as!(
        PlatformUserOut,
        "SELECT id, email, name, role, is_active, created_at, updated_at
         FROM platform_users WHERE role = 'superuser' AND id > $1 ORDER BY id LIMIT $2",
        after,
        limit
    )
    .fetch_all(exec)
    .await
}

/// O papel é fixo em `superuser`: nenhuma rota cria owner.
pub async fn insert_superuser(
    conn: &mut PgConnection,
    id: Uuid,
    email: &str,
    name: &str,
    password_hash: &str,
    created_by: Uuid,
) -> Result<PlatformUserOut, sqlx::Error> {
    sqlx::query_as!(
        PlatformUserOut,
        "INSERT INTO platform_users (id, email, name, password_hash, role, created_by)
         VALUES ($1, $2, $3, $4, 'superuser', $5)
         RETURNING id, email, name, role, is_active, created_at, updated_at",
        id,
        email,
        name,
        password_hash,
        created_by
    )
    .fetch_one(conn)
    .await
}

pub async fn update_superuser(
    conn: &mut PgConnection,
    id: Uuid,
    name: Option<&str>,
    password_hash: Option<&str>,
    is_active: Option<bool>,
) -> Result<Option<PlatformUserOut>, sqlx::Error> {
    sqlx::query_as!(
        PlatformUserOut,
        "UPDATE platform_users SET
           name = COALESCE($2, name),
           password_hash = COALESCE($3, password_hash),
           is_active = COALESCE($4, is_active),
           updated_at = now()
         WHERE id = $1 AND role = 'superuser'
         RETURNING id, email, name, role, is_active, created_at, updated_at",
        id,
        name,
        password_hash,
        is_active
    )
    .fetch_optional(conn)
    .await
}

pub async fn delete_superuser(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    let r = sqlx::query!("DELETE FROM platform_users WHERE id = $1 AND role = 'superuser'", id)
        .execute(conn)
        .await?;
    Ok(r.rows_affected() == 1)
}

pub async fn revoke_platform_sessions(conn: &mut PgConnection, user: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE platform_refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user
    )
    .execute(conn)
    .await
    .map(drop)
}
