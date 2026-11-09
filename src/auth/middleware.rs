//! Autorização negada por padrão: toda rota é declarada com um [`Access`] e o
//! roteador aplica [`enforce`] a cada uma. Handlers obtêm o contexto pelas
//! extensões da requisição; sem o middleware, o extractor falha (500), nunca
//! libera acesso.

use std::sync::Arc;

use axum::extract::{RawPathParams, Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::MethodRouter;
use uuid::Uuid;

use crate::access::catalog::Feature;
use crate::access::resolver::{PlatformPrincipal, PlatformRole};
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::tenancy::middleware::{TenantCtx, resolve_tenant_ctx};

/// Regra de acesso de uma rota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Login, refresh e health checks.
    Public,
    /// Owner ou superusuário ativo (via cache).
    Platform,
    /// Somente o owner; `role` e `is_active` lidos direto do `control`, sem cache.
    PlatformOwner,
    /// Qualquer acesso válido a um tenant, mas só com token de tenant.
    TenantToken,
    /// Exige a feature (owner, superusuário e admin do tenant passam).
    Feature(Feature),
    /// Exige a feature, exceto quando o `{id}` da rota é o próprio usuário.
    SelfOr(Feature),
}

impl Access {
    pub fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

/// Definição de rota com o acesso exigido declarado.
pub struct RouteDef {
    pub method: Method,
    pub path: &'static str,
    pub access: Access,
    pub handler: MethodRouter<AppState>,
}

pub fn route(method: Method, path: &'static str, access: Access, handler: MethodRouter<AppState>) -> RouteDef {
    RouteDef {
        method,
        path,
        access,
        handler,
    }
}

/// Usuário de plataforma autenticado.
#[derive(Clone, Debug)]
pub struct PlatformActor {
    pub id: Uuid,
    pub role: PlatformRole,
}

pub fn bearer(headers: &HeaderMap) -> AppResult<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .ok_or(AppError::Unauthorized)
}

/// Token de plataforma + papel/status pelo cache.
pub async fn platform_actor(state: &AppState, headers: &HeaderMap) -> AppResult<PlatformActor> {
    let claims = platform_claims(state, headers)?;
    let p = state.platform.get(claims.sub, &state.control).await?;
    match p.as_deref() {
        Some(PlatformPrincipal { role, is_active: true }) => Ok(PlatformActor {
            id: claims.sub,
            role: *role,
        }),
        _ => Err(AppError::Unauthorized),
    }
}

/// Claims de plataforma. Um token de tenant válido recebe 403: está
/// autenticado, mas nunca acessa rotas de plataforma.
fn platform_claims(state: &AppState, headers: &HeaderMap) -> AppResult<crate::auth::jwt::PlatformClaims> {
    let token = bearer(headers)?;
    match state.jwt.verify_platform(token) {
        Some(c) => Ok(c),
        None if state.jwt.verify_tenant(token).is_some() => {
            Err(AppError::Forbidden("tenant tokens cannot access platform routes"))
        }
        None => Err(AppError::Unauthorized),
    }
}

/// Owner verificado direto no `control`, sem cache.
async fn owner_actor(state: &AppState, headers: &HeaderMap) -> AppResult<PlatformActor> {
    let claims = platform_claims(state, headers)?;
    let row = sqlx::query!("SELECT role, is_active FROM platform_users WHERE id = $1", claims.sub)
        .fetch_optional(&state.control)
        .await?;
    match row {
        Some(r) if r.is_active && r.role == "owner" => Ok(PlatformActor {
            id: claims.sub,
            role: PlatformRole::Owner,
        }),
        Some(r) if r.is_active => Err(AppError::Forbidden("owner only")),
        _ => Err(AppError::Unauthorized),
    }
}

pub async fn enforce(
    State((state, access)): State<(AppState, Access)>,
    params: RawPathParams,
    mut req: Request,
    next: Next,
) -> Result<Response, AppError> {
    match access {
        Access::Public => {}
        Access::Platform => {
            let actor = platform_actor(&state, req.headers()).await?;
            req.extensions_mut().insert(actor);
        }
        Access::PlatformOwner => {
            let actor = owner_actor(&state, req.headers()).await?;
            req.extensions_mut().insert(actor);
        }
        Access::TenantToken | Access::Feature(_) | Access::SelfOr(_) => {
            let ctx = resolve_tenant_ctx(&state, req.headers(), access == Access::TenantToken).await?;
            match access {
                Access::Feature(f) if !ctx.can(f) => return Err(AppError::Forbidden("missing feature")),
                Access::SelfOr(f) if !ctx.can(f) => {
                    let id = params.iter().find(|(k, _)| *k == "id").map(|(_, v)| v);
                    let is_self = id.and_then(|v| Uuid::parse_str(v).ok()) == Some(ctx.actor_id());
                    if !is_self {
                        return Err(AppError::Forbidden("missing feature"));
                    }
                }
                _ => {}
            }
            req.extensions_mut().insert(Arc::new(ctx));
        }
    }
    Ok(next.run(req).await)
}

/// Extractor do contexto de tenant inserido por [`enforce`].
pub struct Tenant(pub Arc<TenantCtx>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Tenant {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut axum::http::request::Parts, _: &S) -> Result<Self, AppError> {
        parts
            .extensions
            .get::<Arc<TenantCtx>>()
            .cloned()
            .map(Tenant)
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("rota de tenant sem regra de acesso")))
    }
}

/// Extractor do usuário de plataforma inserido por [`enforce`].
pub struct Platform(pub PlatformActor);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for Platform {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut axum::http::request::Parts, _: &S) -> Result<Self, AppError> {
        parts
            .extensions
            .get::<PlatformActor>()
            .cloned()
            .map(Platform)
            .ok_or_else(|| AppError::Internal(anyhow::anyhow!("rota de plataforma sem regra de acesso")))
    }
}
