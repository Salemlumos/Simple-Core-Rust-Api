//! Queries de usuários do tenant. Toda função recebe explicitamente o
//! executor do banco do tenant da requisição.

use sqlx::{PgConnection, PgExecutor};
use uuid::Uuid;

use super::model::UserOut;

pub struct NewUser<'a> {
    pub id: Uuid,
    pub email: &'a str,
    pub name: &'a str,
    pub password_hash: &'a str,
    pub is_tenant_admin: bool,
}

pub async fn insert(conn: &mut PgConnection, u: NewUser<'_>) -> Result<UserOut, sqlx::Error> {
    sqlx::query_as!(
        UserOut,
        "INSERT INTO users (id, email, name, password_hash, is_tenant_admin)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, email, name, is_tenant_admin, is_active, created_at, updated_at",
        u.id,
        u.email,
        u.name,
        u.password_hash,
        u.is_tenant_admin
    )
    .fetch_one(conn)
    .await
}

pub async fn get<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> Result<Option<UserOut>, sqlx::Error> {
    sqlx::query_as!(
        UserOut,
        "SELECT id, email, name, is_tenant_admin, is_active, created_at, updated_at FROM users WHERE id = $1",
        id
    )
    .fetch_optional(exec)
    .await
}

/// Paginação por keyset: `WHERE id > $1 ORDER BY id LIMIT $2`.
pub async fn list<'e, E: PgExecutor<'e>>(exec: E, after: Uuid, limit: i64) -> Result<Vec<UserOut>, sqlx::Error> {
    sqlx::query_as!(
        UserOut,
        "SELECT id, email, name, is_tenant_admin, is_active, created_at, updated_at
         FROM users WHERE id > $1 ORDER BY id LIMIT $2",
        after,
        limit
    )
    .fetch_all(exec)
    .await
}

pub async fn list_admins<'e, E: PgExecutor<'e>>(exec: E, after: Uuid, limit: i64) -> Result<Vec<UserOut>, sqlx::Error> {
    sqlx::query_as!(
        UserOut,
        "SELECT id, email, name, is_tenant_admin, is_active, created_at, updated_at
         FROM users WHERE is_tenant_admin AND id > $1 ORDER BY id LIMIT $2",
        after,
        limit
    )
    .fetch_all(exec)
    .await
}

pub struct UserPatch<'a> {
    pub name: Option<&'a str>,
    pub email: Option<&'a str>,
    pub password_hash: Option<&'a str>,
    pub is_active: Option<bool>,
}

/// `UPDATE ... RETURNING` numa única query. Admins só são alterados se
/// `allow_admin` (usuário de plataforma, ou o próprio admin em nome/senha).
pub async fn update(
    conn: &mut PgConnection,
    id: Uuid,
    p: &UserPatch<'_>,
    allow_admin: bool,
) -> Result<Option<UserOut>, sqlx::Error> {
    sqlx::query_as!(
        UserOut,
        "UPDATE users SET
           name = COALESCE($2, name),
           email = COALESCE($3, email),
           password_hash = COALESCE($4, password_hash),
           is_active = COALESCE($5, is_active),
           updated_at = now()
         WHERE id = $1 AND (NOT is_tenant_admin OR $6)
         RETURNING id, email, name, is_tenant_admin, is_active, created_at, updated_at",
        id,
        p.name,
        p.email,
        p.password_hash,
        p.is_active,
        allow_admin
    )
    .fetch_optional(conn)
    .await
}

pub struct Deleted {
    pub was_active_admin: bool,
}

pub async fn delete(conn: &mut PgConnection, id: Uuid, allow_admin: bool) -> Result<Option<Deleted>, sqlx::Error> {
    let row = sqlx::query_scalar!(
        r#"DELETE FROM users WHERE id = $1 AND (NOT is_tenant_admin OR $2)
           RETURNING (is_tenant_admin AND is_active) AS "was_active_admin!""#,
        id,
        allow_admin
    )
    .fetch_optional(conn)
    .await?;
    Ok(row.map(|was_active_admin| Deleted { was_active_admin }))
}

pub async fn is_admin<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar!("SELECT is_tenant_admin FROM users WHERE id = $1", id)
        .fetch_optional(exec)
        .await
}

pub async fn password_hash<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!("SELECT password_hash FROM users WHERE id = $1", id)
        .fetch_optional(exec)
        .await
}

/// Serializa alterações que podem remover o último admin ativo e verifica se
/// ainda resta algum. Precisa rodar depois da alteração, na mesma transação:
/// em READ COMMITTED, a consulta após o lock enxerga o que as transações
/// concorrentes já confirmaram.
pub async fn lock_and_any_active_admin(conn: &mut PgConnection) -> Result<bool, sqlx::Error> {
    sqlx::query!("SELECT pg_advisory_xact_lock(hashtextextended('simple-core:tenant-admins', 0))")
        .execute(&mut *conn)
        .await?;
    sqlx::query_scalar!(r#"SELECT EXISTS (SELECT 1 FROM users WHERE is_tenant_admin AND is_active) AS "any!""#)
        .fetch_one(conn)
        .await
}

pub async fn revoke_sessions(conn: &mut PgConnection, user: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
        user
    )
    .execute(conn)
    .await
    .map(drop)
}

pub async fn set_admin(conn: &mut PgConnection, id: Uuid, admin: bool) -> Result<bool, sqlx::Error> {
    let r = sqlx::query!(
        "UPDATE users SET is_tenant_admin = $2, updated_at = now() WHERE id = $1 AND is_tenant_admin <> $2",
        id,
        admin
    )
    .execute(conn)
    .await?;
    Ok(r.rows_affected() == 1)
}
