//! Endpoints de plataforma. Tokens de tenant nunca chegam aqui (o middleware
//! só aceita tokens com a audiência de plataforma).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use super::repository::{self as repo, PlatformUserOut, TenantOut};
use crate::access::repository::audit_as;
use crate::auth::middleware::Platform;
use crate::db::{is_unique_violation, is_valid_slug, tenant_db_name};
use crate::error::{ApiJson, ApiPath, ApiQuery, AppError, AppResult, field, reason};
use crate::openapi::Problem;
use crate::state::{AppState, Invalidation};
use crate::tenancy::registry::{TenantInfo, TenantStatus};
use crate::users::handlers::{CreateUser, ListQuery};
use crate::users::model::{Page, UserOut, Validator, page, page_limit};
use crate::users::repository::{self as users_repo, NewUser};

// ---------------------------------------------------------------- me

/// Dados do usuário de plataforma logado.
#[utoipa::path(get, path = "/v1/platform/me", tag = "plataforma",
    responses((status = 200, description = "Usuário logado", body = PlatformUserOut)))]
pub async fn me(State(st): State<AppState>, Platform(actor): Platform) -> AppResult<Json<PlatformUserOut>> {
    repo::get_platform_user(&st.control, actor.id)
        .await?
        .map(Json)
        .ok_or(AppError::Unauthorized)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchMe {
    pub name: Option<String>,
    pub password: Option<String>,
    pub current_password: Option<String>,
}

/// Altera o próprio nome ou a própria senha (exige `current_password`).
///
/// Trocar a senha revoga todas as sessões.
#[utoipa::path(patch, path = "/v1/platform/me", tag = "plataforma",
    request_body = PatchMe,
    responses(
        (status = 200, description = "Usuário atualizado", body = PlatformUserOut),
        (status = 403, description = "Senha atual inválida", body = Problem, content_type = "application/problem+json"),
    ))]
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
            v.err("current_password", field::REQUIRED, "required to change your password");
        }
    } else if b.current_password.is_some() {
        v.err(
            "current_password",
            field::NOT_ALLOWED,
            "only allowed together with password",
        );
    }
    if b.name.is_none() && b.password.is_none() {
        v.err("body", field::NO_CHANGES, "no fields to update");
    }
    v.finish()?;
    let hash = match (b.password, b.current_password) {
        (Some(p), Some(current)) => {
            let stored = repo::platform_password_hash(&st.control, actor.id).await?;
            if !st.hasher.verify(current, stored).await? {
                return Err(AppError::Forbidden(reason::INVALID_CURRENT_PASSWORD));
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

/// Lista os tenants (paginação por cursor).
#[utoipa::path(get, path = "/v1/platform/tenants", tag = "plataforma: tenants",
    params(ListQuery),
    responses((status = 200, description = "Página de tenants", body = Page<TenantOut>)))]
pub async fn list_tenants(
    State(st): State<AppState>,
    _: Platform,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> AppResult<Json<Page<TenantOut>>> {
    let limit = page_limit(q.limit)?;
    let items = repo::list_tenants(&st.control, q.after.unwrap_or(Uuid::nil()), limit).await?;
    Ok(Json(page(items, limit, |t| t.id)))
}

/// Busca um tenant pelo id (inclui status e último erro de provisionamento).
#[utoipa::path(get, path = "/v1/platform/tenants/{id}", tag = "plataforma: tenants",
    params(("id" = Uuid, Path, description = "Id do tenant")),
    responses((status = 200, description = "Tenant", body = TenantOut)))]
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

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateTenant {
    /// Usado no header `X-Tenant-Slug`: `^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$`.
    pub slug: String,
    pub name: String,
}

/// Grava o tenant como `provisioning` e retorna 202; o worker cria o banco.
#[utoipa::path(post, path = "/v1/platform/tenants", tag = "plataforma: tenants",
    request_body = CreateTenant,
    responses(
        (status = 202, description = "Tenant registrado como `provisioning`; o worker cria o banco", body = TenantOut),
        (status = 409, description = "Slug já usado", body = Problem, content_type = "application/problem+json"),
    ))]
pub async fn create_tenant(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiJson(b): ApiJson<CreateTenant>,
) -> AppResult<(StatusCode, Json<TenantOut>)> {
    let mut v = Validator::default();
    let name = v.name("name", &b.name);
    if !is_valid_slug(&b.slug) {
        v.err(
            "slug",
            field::INVALID_FORMAT,
            "must match ^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$",
        );
    }
    v.finish()?;
    let id = Uuid::now_v7();
    let mut tx = st.control.begin().await?;
    let t = repo::insert_tenant(&mut tx, id, &b.slug, &name, &tenant_db_name(id), actor.id)
        .await
        .map_err(|e| {
            if is_unique_violation(&e) {
                AppError::Conflict(reason::SLUG_TAKEN)
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

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchTenant {
    pub name: Option<String>,
    /// `active` ou `suspended`.
    #[schema(example = "suspended")]
    pub status: Option<String>,
}

/// Renomeia, suspende ou reativa um tenant.
///
/// A suspensão vale para todas as instâncias da API em poucos milissegundos.
#[utoipa::path(patch, path = "/v1/platform/tenants/{id}", tag = "plataforma: tenants",
    params(("id" = Uuid, Path, description = "Id do tenant")),
    request_body = PatchTenant,
    responses(
        (status = 200, description = "Tenant atualizado", body = TenantOut),
        (status = 409, description = "O status só muda entre `active` e `suspended`", body = Problem, content_type = "application/problem+json"),
    ))]
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
        v.err("status", field::INVALID_CHOICE, "must be active or suspended");
    }
    if b.name.is_none() && b.status.is_none() {
        v.err("body", field::NO_CHANGES, "no fields to update");
    }
    v.finish()?;
    let mut tx = st.control.begin().await?;
    let Some(t) = repo::update_tenant(&mut tx, id, name.as_deref(), b.status.as_deref()).await? else {
        return Err(match repo::get_tenant(&mut *tx, id).await? {
            Some(_) => AppError::Conflict(reason::TENANT_STATUS_TRANSITION),
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

/// Retoma o provisionamento de um tenant com status `failed`.
#[utoipa::path(post, path = "/v1/platform/tenants/{id}/retry", tag = "plataforma: tenants",
    params(("id" = Uuid, Path, description = "Id do tenant")),
    responses(
        (status = 202, description = "Tenant voltou para `provisioning`", body = TenantOut),
        (status = 409, description = "O tenant não está `failed`", body = Problem, content_type = "application/problem+json"),
    ))]
pub async fn retry_tenant(
    State(st): State<AppState>,
    Platform(actor): Platform,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<(StatusCode, Json<TenantOut>)> {
    let mut tx = st.control.begin().await?;
    let Some(t) = repo::retry_tenant(&mut tx, id).await? else {
        return Err(match repo::get_tenant(&mut *tx, id).await? {
            Some(_) => AppError::Conflict(reason::TENANT_NOT_FAILED),
            None => AppError::NotFound,
        });
    };
    repo::audit(&mut tx, actor.id, "tenant.retry", "tenant", id, json!({})).await?;
    tx.commit().await?;
    Ok((StatusCode::ACCEPTED, Json(t)))
}

// ---------------------------------------------------------------- superusuários

/// Lista os superusuários.
#[utoipa::path(get, path = "/v1/platform/superusers", tag = "plataforma: superusuários",
    params(ListQuery),
    responses((status = 200, description = "Página de superusuários", body = Page<PlatformUserOut>)))]
pub async fn list_superusers(
    State(st): State<AppState>,
    _: Platform,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> AppResult<Json<Page<PlatformUserOut>>> {
    let limit = page_limit(q.limit)?;
    let items = repo::list_superusers(&st.control, q.after.unwrap_or(Uuid::nil()), limit).await?;
    Ok(Json(page(items, limit, |u| u.id)))
}

/// Cria um superusuário.
#[utoipa::path(post, path = "/v1/platform/superusers", tag = "plataforma: superusuários",
    request_body = CreateUser,
    responses(
        (status = 201, description = "Superusuário criado", body = PlatformUserOut),
        (status = 409, description = "E-mail já usado", body = Problem, content_type = "application/problem+json"),
    ))]
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
                AppError::Conflict(reason::EMAIL_TAKEN)
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

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchSuperuser {
    pub name: Option<String>,
    pub password: Option<String>,
    pub is_active: Option<bool>,
}

/// Altera nome, senha ou status de um superusuário.
///
/// Trocar a senha ou desativar revoga as sessões dele.
#[utoipa::path(patch, path = "/v1/platform/superusers/{id}", tag = "plataforma: superusuários",
    params(("id" = Uuid, Path, description = "Id do superusuário")),
    request_body = PatchSuperuser,
    responses((status = 200, description = "Superusuário atualizado", body = PlatformUserOut)))]
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
        v.err("body", field::NO_CHANGES, "no fields to update");
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

/// Remove um superusuário.
#[utoipa::path(delete, path = "/v1/platform/superusers/{id}", tag = "plataforma: superusuários",
    params(("id" = Uuid, Path, description = "Id do superusuário")),
    responses((status = 204, description = "Superusuário removido")))]
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
        _ => Err(AppError::Conflict(reason::TENANT_NOT_ACTIVE)),
    }
}

/// Lista os admins de um tenant.
#[utoipa::path(get, path = "/v1/platform/tenants/{id}/admins", tag = "plataforma: admins de tenant",
    params(("id" = Uuid, Path, description = "Id do tenant"), ListQuery),
    responses(
        (status = 200, description = "Página de admins", body = Page<UserOut>),
        (status = 409, description = "Tenant não está ativo nem suspenso", body = Problem, content_type = "application/problem+json"),
    ))]
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
#[utoipa::path(post, path = "/v1/platform/tenants/{id}/admins", tag = "plataforma: admins de tenant",
    params(("id" = Uuid, Path, description = "Id do tenant")),
    request_body = CreateUser,
    responses(
        (status = 201, description = "Admin criado", body = UserOut),
        (status = 409, description = "E-mail já usado ou tenant não está ativo", body = Problem, content_type = "application/problem+json"),
    ))]
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
            AppError::Conflict(reason::EMAIL_TAKEN)
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
#[utoipa::path(delete, path = "/v1/platform/tenants/{id}/admins/{user_id}", tag = "plataforma: admins de tenant",
    params(
        ("id" = Uuid, Path, description = "Id do tenant"),
        ("user_id" = Uuid, Path, description = "Id do usuário admin"),
    ),
    responses(
        (status = 204, description = "O usuário deixou de ser admin"),
        (status = 409, description = "Tenant não está ativo nem suspenso", body = Problem, content_type = "application/problem+json"),
    ))]
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
