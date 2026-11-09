//! Login, refresh e logout de tenant e de plataforma.
//!
//! Respostas de credenciais inválidas são idênticas (401) para tenant
//! inexistente, e-mail inexistente, senha errada ou conta inativa, sempre
//! executando Argon2id (com hash fictício quando não há usuário).

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::middleware::{Platform, Tenant};
use super::ratelimit::Scope;
use super::{ClientIp, new_refresh_token, sha256};
use crate::error::{ApiJson, AppError, AppResult};
use crate::state::AppState;
use crate::tenancy::middleware::{Actor, TENANT_HEADER};
use crate::tenancy::registry::{TenantInfo, TenantStatus};
use crate::users::model::{UserOut, login_email};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefreshReq {
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct TokenResp {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub refresh_token: String,
}

fn refresh_expiry(st: &AppState) -> OffsetDateTime {
    OffsetDateTime::now_utc() + st.cfg.refresh_ttl
}

fn tenant_slug(headers: &HeaderMap) -> AppResult<&str> {
    headers
        .get(TENANT_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AppError::BadRequest("X-Tenant-Slug header required".into()))
}

/// Tenant ativo para login/refresh. Suspenso → 403; inexistente ou não
/// provisionado → `None` (o chamador responde 401).
async fn login_tenant(st: &AppState, slug: &str) -> AppResult<Option<std::sync::Arc<TenantInfo>>> {
    match st.registry.by_slug(slug).await? {
        Some(t) if t.status == TenantStatus::Active => Ok(Some(t)),
        Some(t) if t.status == TenantStatus::Suspended => Err(AppError::Forbidden("tenant suspended")),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------- tenant

pub async fn tenant_login(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(b): ApiJson<LoginReq>,
) -> AppResult<Json<TokenResp>> {
    let slug = tenant_slug(&headers)?;
    let email = login_email(&b.email);
    st.limiter.check(Scope::Tenant(slug), ip, &email).await?;

    let Some(tenant) = login_tenant(&st, slug).await? else {
        st.hasher.verify(b.password, None).await?;
        return Err(AppError::Unauthorized);
    };
    let pool = st.pools.get(&tenant).await?;
    let row = sqlx::query!("SELECT id, password_hash, is_active FROM users WHERE email = $1", email)
        .fetch_optional(&pool.pool)
        .await?;
    let (id, hash, active) = match row {
        Some(r) => (Some(r.id), Some(r.password_hash), r.is_active),
        None => (None, None, false),
    };
    let ok = st.hasher.verify(b.password, hash).await?;
    let id = match id {
        Some(id) if ok && active => id,
        _ => return Err(AppError::Unauthorized),
    };
    let refresh = insert_tenant_refresh(&st, &pool.pool, id).await?;
    Ok(Json(TokenResp {
        access_token: st.jwt.issue_tenant(id, tenant.id)?,
        token_type: "Bearer",
        expires_in: st.jwt.ttl_secs(),
        refresh_token: refresh,
    }))
}

async fn insert_tenant_refresh<'e, E: sqlx::PgExecutor<'e>>(st: &AppState, exec: E, user: Uuid) -> AppResult<String> {
    let (token, hash) = new_refresh_token();
    sqlx::query!(
        "INSERT INTO refresh_tokens (id, user_id, token_hash, expires_at) VALUES ($1, $2, $3, $4)",
        Uuid::now_v7(),
        user,
        hash,
        refresh_expiry(st)
    )
    .execute(exec)
    .await?;
    Ok(token)
}

pub async fn tenant_refresh(
    State(st): State<AppState>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<RefreshReq>,
) -> AppResult<Json<TokenResp>> {
    let slug = tenant_slug(&headers)?;
    let tenant = login_tenant(&st, slug).await?.ok_or(AppError::Unauthorized)?;
    let pool = st.pools.get(&tenant).await?;
    let hash = sha256(&b.refresh_token);
    let mut tx = pool.pool.begin().await?;
    let rotated = sqlx::query_scalar!(
        "UPDATE refresh_tokens rt SET revoked_at = now()
         FROM users u
         WHERE rt.token_hash = $1 AND rt.revoked_at IS NULL AND rt.expires_at > now()
           AND u.id = rt.user_id AND u.is_active
         RETURNING rt.user_id",
        hash
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(user) = rotated else {
        let prev = sqlx::query!(
            r#"SELECT user_id, revoked_at IS NOT NULL AS "revoked!" FROM refresh_tokens WHERE token_hash = $1"#,
            hash
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(p) = prev.filter(|p| p.revoked) {
            // Reuso de token revogado: encerra todas as sessões do usuário.
            sqlx::query!(
                "UPDATE refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
                p.user_id
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            metrics::counter!("refresh_token_reuse_total", "scope" => "tenant").increment(1);
            tracing::warn!(tenant = %tenant.id, user = %p.user_id, "reuso de refresh token: sessões revogadas");
        }
        return Err(AppError::Unauthorized);
    };
    let refresh = insert_tenant_refresh(&st, &mut *tx, user).await?;
    tx.commit().await?;
    Ok(Json(TokenResp {
        access_token: st.jwt.issue_tenant(user, tenant.id)?,
        token_type: "Bearer",
        expires_in: st.jwt.ttl_secs(),
        refresh_token: refresh,
    }))
}

pub async fn tenant_logout(Tenant(ctx): Tenant, ApiJson(b): ApiJson<RefreshReq>) -> AppResult<StatusCode> {
    sqlx::query!(
        "UPDATE refresh_tokens SET revoked_at = now()
         WHERE token_hash = $1 AND user_id = $2 AND revoked_at IS NULL",
        sha256(&b.refresh_token),
        ctx.actor_id()
    )
    .execute(ctx.db())
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct TenantRef {
    pub id: Uuid,
    pub slug: String,
}

#[derive(Serialize)]
pub struct MeResp {
    #[serde(flatten)]
    pub user: UserOut,
    pub tenant: TenantRef,
    pub features: Vec<&'static str>,
}

pub async fn tenant_me(Tenant(ctx): Tenant) -> AppResult<Json<MeResp>> {
    let Actor::Tenant { id, principal } = &ctx.actor else {
        return Err(AppError::Forbidden("tenant token required"));
    };
    let user = sqlx::query_as!(
        UserOut,
        "SELECT id, email, name, is_tenant_admin, is_active, created_at, updated_at FROM users WHERE id = $1",
        id
    )
    .fetch_optional(ctx.db())
    .await?
    .ok_or(AppError::Unauthorized)?;
    let features = if principal.is_tenant_admin {
        crate::access::catalog::FeatureSet::all()
    } else {
        principal.features
    };
    Ok(Json(MeResp {
        user,
        tenant: TenantRef {
            id: ctx.tenant.id,
            slug: ctx.tenant.slug.clone(),
        },
        features: features.codes(),
    }))
}

// ---------------------------------------------------------------- plataforma

pub async fn platform_login(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    ApiJson(b): ApiJson<LoginReq>,
) -> AppResult<Json<TokenResp>> {
    let email = login_email(&b.email);
    st.limiter.check(Scope::Platform, ip, &email).await?;
    let row = sqlx::query!(
        "SELECT id, password_hash, is_active FROM platform_users WHERE email = $1",
        email
    )
    .fetch_optional(&st.control)
    .await?;
    let (id, hash, active) = match row {
        Some(r) => (Some(r.id), Some(r.password_hash), r.is_active),
        None => (None, None, false),
    };
    let ok = st.hasher.verify(b.password, hash).await?;
    let id = match id {
        Some(id) if ok && active => id,
        _ => return Err(AppError::Unauthorized),
    };
    let refresh = insert_platform_refresh(&st, &st.control, id).await?;
    Ok(Json(TokenResp {
        access_token: st.jwt.issue_platform(id)?,
        token_type: "Bearer",
        expires_in: st.jwt.ttl_secs(),
        refresh_token: refresh,
    }))
}

async fn insert_platform_refresh<'e, E: sqlx::PgExecutor<'e>>(st: &AppState, exec: E, user: Uuid) -> AppResult<String> {
    let (token, hash) = new_refresh_token();
    sqlx::query!(
        "INSERT INTO platform_refresh_tokens (id, user_id, token_hash, expires_at) VALUES ($1, $2, $3, $4)",
        Uuid::now_v7(),
        user,
        hash,
        refresh_expiry(st)
    )
    .execute(exec)
    .await?;
    Ok(token)
}

pub async fn platform_refresh(
    State(st): State<AppState>,
    ApiJson(b): ApiJson<RefreshReq>,
) -> AppResult<Json<TokenResp>> {
    let hash = sha256(&b.refresh_token);
    let control: &PgPool = &st.control;
    let mut tx = control.begin().await?;
    let rotated = sqlx::query_scalar!(
        "UPDATE platform_refresh_tokens rt SET revoked_at = now()
         FROM platform_users u
         WHERE rt.token_hash = $1 AND rt.revoked_at IS NULL AND rt.expires_at > now()
           AND u.id = rt.user_id AND u.is_active
         RETURNING rt.user_id",
        hash
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(user) = rotated else {
        let prev = sqlx::query!(
            r#"SELECT user_id, revoked_at IS NOT NULL AS "revoked!" FROM platform_refresh_tokens WHERE token_hash = $1"#,
            hash
        )
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(p) = prev.filter(|p| p.revoked) {
            sqlx::query!(
                "UPDATE platform_refresh_tokens SET revoked_at = now() WHERE user_id = $1 AND revoked_at IS NULL",
                p.user_id
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            metrics::counter!("refresh_token_reuse_total", "scope" => "platform").increment(1);
            tracing::warn!(user = %p.user_id, "reuso de refresh token de plataforma: sessões revogadas");
        }
        return Err(AppError::Unauthorized);
    };
    let refresh = insert_platform_refresh(&st, &mut *tx, user).await?;
    tx.commit().await?;
    Ok(Json(TokenResp {
        access_token: st.jwt.issue_platform(user)?,
        token_type: "Bearer",
        expires_in: st.jwt.ttl_secs(),
        refresh_token: refresh,
    }))
}

pub async fn platform_logout(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiJson(b): ApiJson<RefreshReq>,
) -> AppResult<StatusCode> {
    sqlx::query!(
        "UPDATE platform_refresh_tokens SET revoked_at = now()
         WHERE token_hash = $1 AND user_id = $2 AND revoked_at IS NULL",
        sha256(&b.refresh_token),
        actor.id
    )
    .execute(&st.control)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
