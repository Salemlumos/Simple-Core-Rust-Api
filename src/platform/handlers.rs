//! Endpoints de plataforma. Tokens de tenant nunca chegam aqui (o middleware
//! só aceita tokens com a audiência de plataforma).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::repository::{self as repo, PlatformUserOut, TenantOut};
use crate::access::repository::audit_as;
use crate::auth::middleware::Platform;
use crate::db::{is_unique_violation, is_valid_slug, tenant_db_name};
use crate::error::{ApiJson, ApiPath, ApiQuery, AppError, AppResult};
use crate::state::{AppState, Invalidation};
use crate::tenancy::registry::{TenantInfo, TenantStatus};
use crate::users::handlers::{CreateUser, ListQuery};
use crate::users::model::{Page, UserOut, Validator, page, page_limit};
use crate::users::repository::{self as users_repo, NewUser};

// ---------------------------------------------------------------- me

pub async fn me(State(st): State<AppState>, Platform(actor): Platform) -> AppResult<Json<PlatformUserOut>> {
    repo::get_platform_user(&st.control, actor.id)
        .await?
        .map(Json)
        .ok_or(AppError::Unauthorized)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchMe {
    pub name: Option<String>,
    pub password: Option<String>,
    pub current_password: Option<String>,
}

pub async fn patch_me(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiJson(b): ApiJson<PatchMe>,
) -> AppResult<Json<PlatformUserOut>> {
    let mut v = Validator::default();
    let name = b.name.as_deref().map(|n| v.name("name", n));
    if let Some(p) = &b.password {
        v.password("password", p);
        if b.current_password.is_none() {
            v.err("current_password", "required to change your password");
        }
    } else if b.current_password.is_some() {
        v.err("current_password", "only allowed together with password");
    }
    if b.name.is_none() && b.password.is_none() {
        v.err("body", "no fields to update");
    }
    v.finish()?;
    let hash = match (b.password, b.current_password) {
        (Some(p), Some(current)) => {
            let stored = repo::platform_password_hash(&st.control, actor.id).await?;
            if !st.hasher.verify(current, stored).await? {
                return Err(AppError::Forbidden("current password is invalid"));
            }
            Some(st.hasher.hash(p).await?)
        }
        _ => None,
    };
    let mut tx = st.control.begin().await?;
    let out = repo::update_me(&mut tx, actor.id, name.as_deref(), hash.as_deref())
        .await?
        .ok_or(AppError::Unauthorized)?;
    if hash.is_some() {
        repo::revoke_platform_sessions(&mut tx, actor.id).await?;
    }
    repo::audit(
        &mut tx,
        actor.id,
        "platform_user.update_self",
        "platform_user",
        actor.id,
        json!({ "name": name, "password_changed": hash.is_some() }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(out))
}

// ---------------------------------------------------------------- tenants

pub async fn list_tenants(
    State(st): State<AppState>,
    _: Platform,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> AppResult<Json<Page<TenantOut>>> {
    let limit = page_limit(q.limit)?;
    let items = repo::list_tenants(&st.control, q.after.unwrap_or(Uuid::nil()), limit).await?;
    Ok(Json(page(items, limit, |t| t.id)))
}

pub async fn get_tenant(
    State(st): State<AppState>,
    _: Platform,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<Json<TenantOut>> {
    repo::get_tenant(&st.control, id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateTenant {
    pub slug: String,
    pub name: String,
}

/// Grava o tenant como `provisioning` e retorna 202; o worker cria o banco.
pub async fn create_tenant(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiJson(b): ApiJson<CreateTenant>,
) -> AppResult<(StatusCode, Json<TenantOut>)> {
    let mut v = Validator::default();
    let name = v.name("name", &b.name);
    if !is_valid_slug(&b.slug) {
        v.err("slug", "must match ^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$");
    }
    v.finish()?;
    let id = Uuid::now_v7();
    let mut tx = st.control.begin().await?;
    let t = repo::insert_tenant(&mut tx, id, &b.slug, &name, &tenant_db_name(id), actor.id)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::Conflict("slug already in use")
            } else {
                e.into()
            }
        })?;
    repo::audit(
        &mut tx,
        actor.id,
        "tenant.create",
        "tenant",
        id,
        json!({ "slug": t.slug, "name": t.name }),
    )
    .await?;
    tx.commit().await?;
    st.apply(&Invalidation::Tenant {
        id,
        slug: t.slug.clone(),
    })
    .await;
    Ok((StatusCode::ACCEPTED, Json(t)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchTenant {
    pub name: Option<String>,
    pub status: Option<String>,
}

pub async fn patch_tenant(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PatchTenant>,
) -> AppResult<Json<TenantOut>> {
    let mut v = Validator::default();
    let name = b.name.as_deref().map(|n| v.name("name", n));
    if let Some(s) = &b.status
        && s != "active"
        && s != "suspended"
    {
        v.err("status", "must be active or suspended");
    }
    if b.name.is_none() && b.status.is_none() {
        v.err("body", "no fields to update");
    }
    v.finish()?;
    let mut tx = st.control.begin().await?;
    let Some(t) = repo::update_tenant(&mut tx, id, name.as_deref(), b.status.as_deref()).await? else {
        return Err(match repo::get_tenant(&mut *tx, id).await? {
            Some(_) => AppError::Conflict("tenant status can only change between active and suspended"),
            None => AppError::NotFound,
        });
    };
    repo::audit(
        &mut tx,
        actor.id,
        "tenant.update",
        "tenant",
        id,
        json!({ "name": name, "status": b.status }),
    )
    .await?;
    tx.commit().await?;
    // O trigger também notifica as outras instâncias.
    st.apply(&Invalidation::Tenant {
        id,
        slug: t.slug.clone(),
    })
    .await;
    Ok(Json(t))
}

pub async fn retry_tenant(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<(StatusCode, Json<TenantOut>)> {
    let mut tx = st.control.begin().await?;
    let Some(t) = repo::retry_tenant(&mut tx, id).await? else {
        return Err(match repo::get_tenant(&mut *tx, id).await? {
            Some(_) => AppError::Conflict("only failed tenants can be retried"),
            None => AppError::NotFound,
        });
    };
    repo::audit(&mut tx, actor.id, "tenant.retry", "tenant", id, json!({})).await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(t)))
}

// ---------------------------------------------------------------- superusuários

pub async fn list_superusers(
    State(st): State<AppState>,
    _: Platform,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> AppResult<Json<Page<PlatformUserOut>>> {
    let limit = page_limit(q.limit)?;
    let items = repo::list_superusers(&st.control, q.after.unwrap_or(Uuid::nil()), limit).await?;
    Ok(Json(page(items, limit, |u| u.id)))
}

pub async fn create_superuser(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiJson(b): ApiJson<CreateUser>,
) -> AppResult<(StatusCode, Json<PlatformUserOut>)> {
    let mut v = Validator::default();
    let email = v.email("email", &b.email);
    let name = v.name("name", &b.name);
    v.password("password", &b.password);
    v.finish()?;
    let hash = st.hasher.hash(b.password).await?;
    let id = Uuid::now_v7();
    let mut tx = st.control.begin().await?;
    let u = repo::insert_superuser(&mut tx, id, &email, &name, &hash, actor.id)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::Conflict("email already in use")
            } else {
                e.into()
            }
        })?;
    repo::audit(
        &mut tx,
        actor.id,
        "superuser.create",
        "platform_user",
        id,
        json!({ "email": u.email }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(u)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchSuperuser {
    pub name: Option<String>,
    pub password: Option<String>,
    pub is_active: Option<bool>,
}

pub async fn patch_superuser(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PatchSuperuser>,
) -> AppResult<Json<PlatformUserOut>> {
    let mut v = Validator::default();
    let name = b.name.as_deref().map(|n| v.name("name", n));
    if let Some(p) = &b.password {
        v.password("password", p);
    }
    if b.name.is_none() && b.password.is_none() && b.is_active.is_none() {
        v.err("body", "no fields to update");
    }
    v.finish()?;
    let hash = match b.password {
        Some(p) => Some(st.hasher.hash(p).await?),
        None => None,
    };
    let mut tx = st.control.begin().await?;
    let u = repo::update_superuser(&mut tx, id, name.as_deref(), hash.as_deref(), b.is_active)
        .await?
        .ok_or(AppError::NotFound)?;
    if hash.is_some() || b.is_active == Some(false) {
        repo::revoke_platform_sessions(&mut tx, id).await?;
    }
    repo::audit(
        &mut tx,
        actor.id,
        "superuser.update",
        "platform_user",
        id,
        json!({ "name": name, "password_changed": hash.is_some(), "is_active": b.is_active }),
    )
    .await?;
    tx.commit().await?;
    st.apply(&Invalidation::PlatformUser { id }).await;
    Ok(Json(u))
}

pub async fn delete_superuser(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<StatusCode> {
    let mut tx = st.control.begin().await?;
    if !repo::delete_superuser(&mut tx, id).await? {
        return Err(AppError::NotFound);
    }
    repo::audit(&mut tx, actor.id, "superuser.delete", "platform_user", id, json!({})).await?;
    tx.commit().await?;
    st.apply(&Invalidation::PlatformUser { id }).await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------- admins de tenant

async fn tenant_for_admins(st: &AppState, id: Uuid, require_active: bool) -> AppResult<std::sync::Arc<TenantInfo>> {
    let t = st.registry.by_id(id).await?.ok_or(AppError::NotFound)?;
    match t.status {
        TenantStatus::Active => Ok(t),
        TenantStatus::Suspended if !require_active => Ok(t),
        _ => Err(AppError::Conflict("tenant is not active")),
    }
}

pub async fn list_admins(
    State(st): State<AppState>,
    _: Platform,
    ApiPath(id): ApiPath<Uuid>,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> AppResult<Json<Page<UserOut>>> {
    let tenant = tenant_for_admins(&st, id, false).await?;
    let pool = st.pools.get(&tenant).await?;
    let limit = page_limit(q.limit)?;
    let items = users_repo::list_admins(&pool.pool, q.after.unwrap_or(Uuid::nil()), limit).await?;
    Ok(Json(page(items, limit, |u| u.id)))
}

/// Cria um novo usuário com `is_tenant_admin = true` (tenant precisa estar ativo).
pub async fn create_admin(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<CreateUser>,
) -> AppResult<(StatusCode, Json<UserOut>)> {
    let mut v = Validator::default();
    let email = v.email("email", &b.email);
    let name = v.name("name", &b.name);
    v.password("password", &b.password);
    v.finish()?;
    let tenant = tenant_for_admins(&st, id, true).await?;
    let pool = st.pools.get(&tenant).await?;
    let hash = st.hasher.hash(b.password).await?;
    let mut tx = pool.pool.begin().await?;
    let user = users_repo::insert(
        &mut tx,
        NewUser {
            id: Uuid::now_v7(),
            email: &email,
            name: &name,
            password_hash: &hash,
            is_tenant_admin: true,
        },
    )
    .await
    .map_err(|e| {
        if is_unique_violation(&e) {
            AppError::Conflict("email already in use")
        } else {
            e.into()
        }
    })?;
    audit_as(
        &mut tx,
        actor.id,
        "platform",
        "tenant_admin.create",
        "user",
        user.id,
        json!({ "email": user.email }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(user)))
}

/// Remove o status de admin, mantendo o usuário.
pub async fn delete_admin(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiPath((id, user_id)): ApiPath<(Uuid, Uuid)>,
) -> AppResult<StatusCode> {
    let tenant = tenant_for_admins(&st, id, false).await?;
    let pool = st.pools.get(&tenant).await?;
    let mut tx = pool.pool.begin().await?;
    if !users_repo::set_admin(&mut tx, user_id, false).await? {
        return Err(AppError::NotFound);
    }
    audit_as(
        &mut tx,
        actor.id,
        "platform",
        "tenant_admin.revoke",
        "user",
        user_id,
        json!({}),
    )
    .await?;
    tx.commit().await?;
    st.publish(Invalidation::TenantUser {
        tid: tenant.id,
        uid: user_id,
    })
    .await;
    Ok(StatusCode::NO_CONTENT)
}
