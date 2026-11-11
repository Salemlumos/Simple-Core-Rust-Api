//! Queries de perfis, exceções e auditoria do tenant.

use serde::Serialize;
use sqlx::{PgConnection, PgExecutor};
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::tenancy::middleware::TenantCtx;

/// Auditoria na mesma transação da alteração.
pub async fn audit(
    conn: &mut PgConnection,
    ctx: &TenantCtx,
    action: &str,
    target_type: &str,
    target_id: impl ToString,
    payload: serde_json::Value,
) -> Result<(), sqlx::Error> {
    audit_as(
        conn,
        ctx.actor_id(),
        ctx.actor_type(),
        action,
        target_type,
        target_id,
        payload,
    )
    .await
}

pub async fn audit_as(
    conn: &mut PgConnection,
    actor_id: Uuid,
    actor_type: &str,
    action: &str,
    target_type: &str,
    target_id: impl ToString,
    payload: serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO access_audit (id, actor_id, actor_type, action, target_type, target_id, payload)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        Uuid::now_v7(),
        actor_id,
        actor_type,
        action,
        target_type,
        target_id.to_string(),
        payload
    )
    .execute(conn)
    .await
    .map(drop)
}

/// Feature de um módulo do catálogo (apenas para a documentação; a lista vem
/// montada em JSON pelo banco).
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct FeatureOut {
    #[schema(example = "users.read")]
    pub code: String,
    pub name: String,
}

#[derive(Serialize, ToSchema)]
pub struct ModuleOut {
    pub code: String,
    pub name: String,
    #[schema(value_type = Vec<FeatureOut>)]
    pub features: serde_json::Value,
}

pub async fn modules<'e, E: PgExecutor<'e>>(exec: E) -> Result<Vec<ModuleOut>, sqlx::Error> {
    sqlx::query_as!(
        ModuleOut,
        r#"SELECT m.code, m.name,
             COALESCE((SELECT jsonb_agg(jsonb_build_object('code', f.code, 'name', f.name) ORDER BY f.code)
                       FROM features f WHERE f.module_id = m.id), '[]'::jsonb) AS "features!"
           FROM modules m ORDER BY m.code"#
    )
    .fetch_all(exec)
    .await
}

#[derive(Serialize, ToSchema)]
pub struct ProfileOut {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub features: Vec<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

pub async fn list_profiles<'e, E: PgExecutor<'e>>(
    exec: E,
    after: Uuid,
    limit: i64,
) -> Result<Vec<ProfileOut>, sqlx::Error> {
    sqlx::query_as!(
        ProfileOut,
        r#"SELECT p.id, p.name, p.description, p.created_at, p.updated_at,
             ARRAY(SELECT f.code FROM profile_features pf JOIN features f ON f.id = pf.feature_id
                   WHERE pf.profile_id = p.id ORDER BY f.code) AS "features!"
           FROM profiles p WHERE p.id > $1 ORDER BY p.id LIMIT $2"#,
        after,
        limit
    )
    .fetch_all(exec)
    .await
}

pub async fn get_profile<'e, E: PgExecutor<'e>>(exec: E, id: Uuid) -> Result<Option<ProfileOut>, sqlx::Error> {
    sqlx::query_as!(
        ProfileOut,
        r#"SELECT p.id, p.name, p.description, p.created_at, p.updated_at,
             ARRAY(SELECT f.code FROM profile_features pf JOIN features f ON f.id = pf.feature_id
                   WHERE pf.profile_id = p.id ORDER BY f.code) AS "features!"
           FROM profiles p WHERE p.id = $1"#,
        id
    )
    .fetch_optional(exec)
    .await
}

pub async fn insert_profile(
    conn: &mut PgConnection,
    id: Uuid,
    name: &str,
    description: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        "INSERT INTO profiles (id, name, description) VALUES ($1, $2, $3)",
        id,
        name,
        description
    )
    .execute(conn)
    .await
    .map(drop)
}

/// `description` vazia limpa o campo.
pub async fn update_profile(
    conn: &mut PgConnection,
    id: Uuid,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<bool, sqlx::Error> {
    let r = sqlx::query!(
        "UPDATE profiles SET
           name = COALESCE($2, name),
           description = CASE WHEN $3::text IS NULL THEN description ELSE NULLIF($3, '') END,
           updated_at = now()
         WHERE id = $1",
        id,
        name,
        description
    )
    .execute(conn)
    .await?;
    Ok(r.rows_affected() == 1)
}

pub async fn delete_profile(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    let r = sqlx::query!("DELETE FROM profiles WHERE id = $1", id)
        .execute(conn)
        .await?;
    Ok(r.rows_affected() == 1)
}

/// Bloqueia o perfil para trocar suas features; `false` se não existe.
pub async fn lock_profile(conn: &mut PgConnection, id: Uuid) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query_scalar!("SELECT id FROM profiles WHERE id = $1 FOR UPDATE", id)
            .fetch_optional(conn)
            .await?
            .is_some(),
    )
}

pub async fn profile_feature_codes(conn: &mut PgConnection, id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT f.code FROM profile_features pf JOIN features f ON f.id = pf.feature_id WHERE pf.profile_id = $1",
        id
    )
    .fetch_all(conn)
    .await
}

pub async fn replace_profile_features(conn: &mut PgConnection, id: Uuid, codes: &[String]) -> Result<(), sqlx::Error> {
    sqlx::query!("DELETE FROM profile_features WHERE profile_id = $1", id)
        .execute(&mut *conn)
        .await?;
    sqlx::query!(
        "INSERT INTO profile_features (profile_id, feature_id)
         SELECT $1, f.id FROM features f WHERE f.code = ANY($2)",
        id,
        codes
    )
    .execute(conn)
    .await
    .map(drop)
}

/// Troca os perfis do usuário e retorna os perfis que foram adicionados.
pub async fn replace_user_profiles(
    conn: &mut PgConnection,
    user: Uuid,
    profiles: &[Uuid],
) -> Result<Vec<Uuid>, sqlx::Error> {
    let old = sqlx::query_scalar!(
        "DELETE FROM user_profiles WHERE user_id = $1 RETURNING profile_id",
        user
    )
    .fetch_all(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO user_profiles (user_id, profile_id) SELECT $1, p FROM unnest($2::uuid[]) AS p",
        user,
        profiles
    )
    .execute(conn)
    .await?;
    Ok(profiles.iter().copied().filter(|p| !old.contains(p)).collect())
}

pub async fn features_of_profiles(conn: &mut PgConnection, profiles: &[Uuid]) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT DISTINCT f.code FROM profile_features pf JOIN features f ON f.id = pf.feature_id
         WHERE pf.profile_id = ANY($1)",
        profiles
    )
    .fetch_all(conn)
    .await
}

#[derive(Serialize, ToSchema)]
pub struct OverrideOut {
    pub feature: String,
    /// `allow` ou `deny` (deny sempre vence).
    pub effect: String,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
    pub created_by: Uuid,
    /// `tenant` ou `platform`.
    pub created_by_type: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub struct NewOverride<'a> {
    pub user: Uuid,
    pub feature: &'a str,
    pub effect: &'a str,
    pub reason: &'a str,
    pub expires_at: Option<OffsetDateTime>,
    pub created_by: Uuid,
    pub created_by_type: &'a str,
}

pub async fn upsert_override(conn: &mut PgConnection, o: NewOverride<'_>) -> Result<Option<OverrideOut>, sqlx::Error> {
    sqlx::query_as!(
        OverrideOut,
        r#"WITH up AS (
             INSERT INTO user_feature_overrides
               (user_id, feature_id, effect, reason, expires_at, created_by, created_by_type)
             SELECT $1, f.id, $3, $4, $5, $6, $7 FROM features f WHERE f.code = $2
             ON CONFLICT (user_id, feature_id) DO UPDATE SET
               effect = EXCLUDED.effect, reason = EXCLUDED.reason, expires_at = EXCLUDED.expires_at,
               created_by = EXCLUDED.created_by, created_by_type = EXCLUDED.created_by_type,
               created_at = now()
             RETURNING effect, reason, expires_at, created_by, created_by_type, created_at
           )
           SELECT $2::text AS "feature!", effect AS "effect!", reason AS "reason!", expires_at,
                  created_by AS "created_by!", created_by_type AS "created_by_type!", created_at AS "created_at!"
           FROM up"#,
        o.user,
        o.feature,
        o.effect,
        o.reason,
        o.expires_at,
        o.created_by,
        o.created_by_type
    )
    .fetch_optional(conn)
    .await
}

/// Remove a exceção e retorna o efeito removido.
pub async fn delete_override(
    conn: &mut PgConnection,
    user: Uuid,
    feature: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar!(
        "DELETE FROM user_feature_overrides o USING features f
         WHERE o.user_id = $1 AND o.feature_id = f.id AND f.code = $2
         RETURNING o.effect",
        user,
        feature
    )
    .fetch_optional(conn)
    .await
}

#[derive(Serialize, ToSchema)]
pub struct ProfileRef {
    pub id: Uuid,
    pub name: String,
}

pub async fn user_profiles<'e, E: PgExecutor<'e>>(exec: E, user: Uuid) -> Result<Vec<ProfileRef>, sqlx::Error> {
    sqlx::query_as!(
        ProfileRef,
        "SELECT p.id, p.name FROM user_profiles up JOIN profiles p ON p.id = up.profile_id
         WHERE up.user_id = $1 ORDER BY p.name",
        user
    )
    .fetch_all(exec)
    .await
}

pub async fn user_overrides<'e, E: PgExecutor<'e>>(exec: E, user: Uuid) -> Result<Vec<OverrideOut>, sqlx::Error> {
    sqlx::query_as!(
        OverrideOut,
        r#"SELECT f.code AS feature, o.effect, o.reason, o.expires_at, o.created_by, o.created_by_type, o.created_at
           FROM user_feature_overrides o JOIN features f ON f.id = o.feature_id
           WHERE o.user_id = $1 ORDER BY f.code"#,
        user
    )
    .fetch_all(exec)
    .await
}
