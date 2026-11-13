//! Login, refresh e logout de tenant e de plataforma.
//!
//! Respostas de credenciais inválidas são idênticas (401) para tenant
//! inexistente, e-mail inexistente, senha errada ou conta inativa, sempre
//! executando Argon2id (com hash fictício quando não há usuário).

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use super::cookie::{self, Kind, RefreshInput, cookie_mode};
use super::middleware::{Platform, Tenant};
use super::ratelimit::Scope;
use super::{ClientIp, new_refresh_token, sha256};
use crate::error::{ApiJson, AppError, AppResult, reason};
use crate::openapi::Problem;
use crate::state::AppState;
use crate::tenancy::middleware::{Actor, TENANT_HEADER};
use crate::tenancy::registry::{TenantInfo, TenantStatus};
use crate::users::model::{UserOut, login_email};

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginReq {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RefreshReq {
    pub refresh_token: String,
}

#[derive(Serialize, ToSchema)]
pub struct TokenResp {
    /// JWT de acesso; envie como `Authorization: Bearer <token>`.
    pub access_token: String,
    #[schema(value_type = String, example = "Bearer")]
    pub token_type: &'static str,
    /// Validade do access token, em segundos.
    pub expires_in: i64,
    /// Omitido com `X-Auth-Mode: cookie`: o token vai no cookie `HttpOnly`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
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
        Some(t) if t.status == TenantStatus::Suspended => Err(AppError::Forbidden(reason::TENANT_SUSPENDED)),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------- tenant

/// Login de usuário de tenant.
///
/// Credenciais inválidas, tenant inexistente e conta inativa respondem o mesmo 401.
#[utoipa::path(post, path = "/v1/auth/login", tag = "autenticação",
    params(
        ("X-Tenant-Slug" = String, Header, description = "Slug do tenant"),
        ("X-Auth-Mode" = Option<String>, Header, description = "`cookie`: grava o refresh token no cookie `HttpOnly` `sc_tenant_rt` e o omite do corpo"),
    ),
    request_body = LoginReq,
    responses(
        (status = 200, description = "Tokens emitidos", body = TokenResp),
        (status = 400, description = "Header X-Tenant-Slug ausente", body = Problem, content_type = "application/problem+json"),
        (status = 401, description = "Credenciais inválidas", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Tenant suspenso", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Limite de tentativas de login", body = Problem, content_type = "application/problem+json",
            headers(("Retry-After" = u64, description = "Segundos até poder tentar de novo"))),
    ))]
pub async fn tenant_login(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(b): ApiJson<LoginReq>,
) -> AppResult<Response> {
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
    let access = st.jwt.issue_tenant(id, tenant.id)?;
    Ok(cookie::tokens(
        &st,
        Kind::Tenant,
        cookie_mode(&headers),
        access,
        refresh,
    ))
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

/// Troca um refresh token de tenant por um novo par de tokens.
///
/// O refresh token usado é revogado. Reusar um token já revogado encerra
/// todas as sessões do usuário.
///
/// Com `X-Auth-Mode: cookie`, o token vem do cookie `sc_tenant_rt` (o corpo é
/// ignorado), o novo token volta no cookie e um 401 apaga o cookie.
#[utoipa::path(post, path = "/v1/auth/refresh", tag = "autenticação",
    params(
        ("X-Tenant-Slug" = String, Header, description = "Slug do tenant"),
        ("X-Auth-Mode" = Option<String>, Header, description = "`cookie`: lê e grava o refresh token no cookie `HttpOnly` `sc_tenant_rt`"),
    ),
    request_body(content = RefreshReq, description = "Obrigatório sem `X-Auth-Mode: cookie`"),
    responses(
        (status = 200, description = "Tokens emitidos", body = TokenResp),
        (status = 401, description = "Refresh token inválido, expirado ou revogado", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Tenant suspenso", body = Problem, content_type = "application/problem+json"),
    ))]
pub async fn tenant_refresh(State(st): State<AppState>, headers: HeaderMap, input: RefreshInput) -> Response {
    let cookie = input.is_cookie();
    match tenant_rotate(&st, &headers, input.token(Kind::Tenant)).await {
        Ok((access, refresh)) => cookie::tokens(&st, Kind::Tenant, cookie, access, refresh),
        Err(e) => cookie::refresh_error(&st, Kind::Tenant, cookie, e),
    }
}

/// Rotaciona o refresh token de tenant; devolve (access, refresh).
async fn tenant_rotate(st: &AppState, headers: &HeaderMap, token: Option<String>) -> AppResult<(String, String)> {
    let slug = tenant_slug(headers)?;
    let token = token.ok_or(AppError::Unauthorized)?;
    let tenant = login_tenant(st, slug).await?.ok_or(AppError::Unauthorized)?;
    let pool = st.pools.get(&tenant).await?;
    let hash = sha256(&token);
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
    let refresh = insert_tenant_refresh(st, &mut *tx, user).await?;
    tx.commit().await?;
    Ok((st.jwt.issue_tenant(user, tenant.id)?, refresh))
}

/// Revoga um refresh token do usuário logado.
///
/// Com `X-Auth-Mode: cookie`, revoga o token do cookie `sc_tenant_rt` (se
/// houver) e apaga o cookie.
#[utoipa::path(post, path = "/v1/auth/logout", tag = "autenticação",
    params(("X-Auth-Mode" = Option<String>, Header, description = "`cookie`: usa e apaga o cookie `sc_tenant_rt`")),
    request_body(content = RefreshReq, description = "Obrigatório sem `X-Auth-Mode: cookie`"),
    responses((status = 204, description = "Refresh token revogado")))]
pub async fn tenant_logout(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    input: RefreshInput,
) -> AppResult<Response> {
    if let Some(token) = input.token(Kind::Tenant) {
        sqlx::query!(
            "UPDATE refresh_tokens SET revoked_at = now()
         WHERE token_hash = $1 AND user_id = $2 AND revoked_at IS NULL",
            sha256(&token),
            ctx.actor_id()
        )
        .execute(ctx.db())
        .await?;
    }
    Ok(cookie::logged_out(&st, Kind::Tenant, input.is_cookie()))
}

#[derive(Serialize, ToSchema)]
pub struct TenantRef {
    pub id: Uuid,
    pub slug: String,
}

#[derive(Serialize, ToSchema)]
pub struct MeResp {
    #[serde(flatten)]
    pub user: UserOut,
    pub tenant: TenantRef,
    /// Códigos das features efetivas (admins recebem todas).
    #[schema(value_type = Vec<String>)]
    pub features: Vec<&'static str>,
}

/// Dados do usuário logado, o tenant e as features efetivas.
#[utoipa::path(get, path = "/v1/me", tag = "autenticação",
    responses((status = 200, description = "Usuário logado", body = MeResp)))]
pub async fn tenant_me(Tenant(ctx): Tenant) -> AppResult<Json<MeResp>> {
    let Actor::Tenant { id, principal } = &ctx.actor else {
        return Err(AppError::Forbidden(reason::TENANT_TOKEN_REQUIRED));
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

/// Login de usuário de plataforma (owner ou superusuário).
#[utoipa::path(post, path = "/v1/platform/auth/login", tag = "plataforma: autenticação",
    params(("X-Auth-Mode" = Option<String>, Header, description = "`cookie`: grava o refresh token no cookie `HttpOnly` `sc_platform_rt` e o omite do corpo")),
    request_body = LoginReq,
    responses(
        (status = 200, description = "Tokens emitidos", body = TokenResp),
        (status = 401, description = "Credenciais inválidas", body = Problem, content_type = "application/problem+json"),
        (status = 429, description = "Limite de tentativas de login", body = Problem, content_type = "application/problem+json",
            headers(("Retry-After" = u64, description = "Segundos até poder tentar de novo"))),
    ))]
pub async fn platform_login(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    ApiJson(b): ApiJson<LoginReq>,
) -> AppResult<Response> {
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
    let access = st.jwt.issue_platform(id)?;
    Ok(cookie::tokens(
        &st,
        Kind::Platform,
        cookie_mode(&headers),
        access,
        refresh,
    ))
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

/// Troca um refresh token de plataforma por um novo par de tokens.
///
/// Reusar um token já revogado encerra todas as sessões do usuário.
///
/// Com `X-Auth-Mode: cookie`, o token vem do cookie `sc_platform_rt` (o corpo
/// é ignorado), o novo token volta no cookie e um 401 apaga o cookie.
#[utoipa::path(post, path = "/v1/platform/auth/refresh", tag = "plataforma: autenticação",
    params(("X-Auth-Mode" = Option<String>, Header, description = "`cookie`: lê e grava o refresh token no cookie `HttpOnly` `sc_platform_rt`")),
    request_body(content = RefreshReq, description = "Obrigatório sem `X-Auth-Mode: cookie`"),
    responses(
        (status = 200, description = "Tokens emitidos", body = TokenResp),
        (status = 401, description = "Refresh token inválido, expirado ou revogado", body = Problem, content_type = "application/problem+json"),
    ))]
pub async fn platform_refresh(State(st): State<AppState>, input: RefreshInput) -> Response {
    let cookie = input.is_cookie();
    match platform_rotate(&st, input.token(Kind::Platform)).await {
        Ok((access, refresh)) => cookie::tokens(&st, Kind::Platform, cookie, access, refresh),
        Err(e) => cookie::refresh_error(&st, Kind::Platform, cookie, e),
    }
}

/// Rotaciona o refresh token de plataforma; devolve (access, refresh).
async fn platform_rotate(st: &AppState, token: Option<String>) -> AppResult<(String, String)> {
    let token = token.ok_or(AppError::Unauthorized)?;
    let hash = sha256(&token);
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
    let refresh = insert_platform_refresh(st, &mut *tx, user).await?;
    tx.commit().await?;
    Ok((st.jwt.issue_platform(user)?, refresh))
}

/// Revoga um refresh token do usuário de plataforma logado.
///
/// Com `X-Auth-Mode: cookie`, revoga o token do cookie `sc_platform_rt` (se
/// houver) e apaga o cookie.
#[utoipa::path(post, path = "/v1/platform/auth/logout", tag = "plataforma: autenticação",
    params(("X-Auth-Mode" = Option<String>, Header, description = "`cookie`: usa e apaga o cookie `sc_platform_rt`")),
    request_body(content = RefreshReq, description = "Obrigatório sem `X-Auth-Mode: cookie`"),
    responses((status = 204, description = "Refresh token revogado")))]
pub async fn platform_logout(
    State(st): State<AppState>,
    Platform(actor): Platform,
    input: RefreshInput,
) -> AppResult<Response> {
    if let Some(token) = input.token(Kind::Platform) {
        sqlx::query!(
            "UPDATE platform_refresh_tokens SET revoked_at = now()
         WHERE token_hash = $1 AND user_id = $2 AND revoked_at IS NULL",
            sha256(&token),
            actor.id
        )
        .execute(&st.control)
        .await?;
    }
    Ok(cookie::logged_out(&st, Kind::Platform, input.is_cookie()))
}
