//! Resolução do tenant da requisição.
//!
//! - Token de tenant: o tenant vem só da claim assinada `tid`; um
//!   `X-Tenant-Slug` divergente retorna 403.
//! - Token de plataforma: exige `X-Tenant-Slug`.
//!
//! Com caches quentes, nada aqui consulta o banco.

use std::sync::Arc;

use axum::http::HeaderMap;
use uuid::Uuid;

use super::pools::TenantPool;
use super::registry::{TenantInfo, TenantStatus};
use crate::access::catalog::{Feature, FeatureSet};
use crate::access::resolver::{PlatformRole, Principal};
use crate::auth::middleware::{bearer, platform_actor};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub const TENANT_HEADER: &str = "x-tenant-slug";

#[derive(Debug, Clone)]
pub enum Actor {
    Platform { id: Uuid, role: PlatformRole },
    Tenant { id: Uuid, principal: Arc<Principal> },
}

pub struct TenantCtx {
    pub tenant: Arc<TenantInfo>,
    pub pool: Arc<TenantPool>,
    pub actor: Actor,
}

impl TenantCtx {
    /// Owner, superusuário e admin do tenant ignoram as verificações de feature.
    pub fn full_access(&self) -> bool {
        match &self.actor {
            Actor::Platform { .. } => true,
            Actor::Tenant { principal, .. } => principal.is_tenant_admin,
        }
    }

    pub fn is_platform(&self) -> bool {
        matches!(self.actor, Actor::Platform { .. })
    }

    #[inline]
    pub fn can(&self, f: Feature) -> bool {
        match &self.actor {
            Actor::Platform { .. } => true,
            Actor::Tenant { principal, .. } => principal.is_tenant_admin || principal.features.contains(f),
        }
    }

    /// Features que o ator pode conceder.
    pub fn grantable(&self) -> FeatureSet {
        match &self.actor {
            Actor::Tenant { principal, .. } if !principal.is_tenant_admin => principal.features,
            _ => FeatureSet::all(),
        }
    }

    pub fn actor_id(&self) -> Uuid {
        match &self.actor {
            Actor::Platform { id, .. } | Actor::Tenant { id, .. } => *id,
        }
    }

    pub fn actor_type(&self) -> &'static str {
        if self.is_platform() { "platform" } else { "tenant" }
    }

    pub fn db(&self) -> &sqlx::PgPool {
        &self.tenant_pool().pool
    }

    fn tenant_pool(&self) -> &TenantPool {
        &self.pool
    }
}

fn header_slug(headers: &HeaderMap) -> AppResult<Option<&str>> {
    match headers.get(TENANT_HEADER) {
        None => Ok(None),
        Some(v) => v
            .to_str()
            .map(Some)
            .map_err(|_| AppError::BadRequest("invalid X-Tenant-Slug".into())),
    }
}

/// Bloqueia tenants não ativos: suspenso → 403 para todos.
fn ensure_active(t: &TenantInfo) -> AppResult<()> {
    match t.status {
        TenantStatus::Active => Ok(()),
        TenantStatus::Suspended => Err(AppError::Forbidden("tenant suspended")),
        _ => Err(AppError::Forbidden("tenant not available")),
    }
}

pub async fn resolve_tenant_ctx(
    state: &AppState,
    headers: &HeaderMap,
    tenant_token_only: bool,
) -> AppResult<TenantCtx> {
    let token = bearer(headers)?;
    let slug = header_slug(headers)?;

    if let Some(claims) = state.jwt.verify_tenant(token) {
        let tenant = state.registry.by_id(claims.tid).await?.ok_or(AppError::Unauthorized)?;
        if let Some(slug) = slug
            && slug != tenant.slug
        {
            return Err(AppError::Forbidden("X-Tenant-Slug does not match token"));
        }
        ensure_active(&tenant)?;
        let pool = state.pools.get(&tenant).await?;
        let principal = state.access.get(tenant.id, claims.sub, &pool.pool).await?;
        let principal = match principal {
            Some(p) if p.is_active => p,
            _ => return Err(AppError::Unauthorized),
        };
        return Ok(TenantCtx {
            tenant,
            pool,
            actor: Actor::Tenant {
                id: claims.sub,
                principal,
            },
        });
    }

    // Não é token de tenant: só resta token de plataforma.
    let actor = platform_actor(state, headers).await?;
    if tenant_token_only {
        return Err(AppError::Forbidden("tenant token required"));
    }
    let slug = slug.ok_or(AppError::Forbidden("X-Tenant-Slug required for platform tokens"))?;
    let tenant = state.registry.by_slug(slug).await?.ok_or(AppError::NotFound)?;
    ensure_active(&tenant)?;
    let pool = state.pools.get(&tenant).await?;
    Ok(TenantCtx {
        tenant,
        pool,
        actor: Actor::Platform {
            id: actor.id,
            role: actor.role,
        },
    })
}
