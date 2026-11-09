//! CRUD de usuários do tenant, protegido por features.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::model::{Page, UserOut, Validator, page, page_limit};
use super::repository::{self as repo, NewUser, UserPatch};
use crate::access::catalog::Feature;
use crate::access::repository::audit;
use crate::auth::middleware::Tenant;
use crate::error::{ApiJson, ApiPath, ApiQuery, AppError, AppResult};
use crate::state::{AppState, Invalidation};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateUser {
    pub email: String,
    pub name: String,
    pub password: String,
}

pub async fn create(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiJson(b): ApiJson<CreateUser>,
) -> AppResult<(StatusCode, Json<UserOut>)> {
    let mut v = Validator::default();
    let email = v.email("email", &b.email);
    let name = v.name("name", &b.name);
    v.password("password", &b.password);
    v.finish()?;
    let hash = st.hasher.hash(b.password).await?;

    let mut tx = ctx.db().begin().await?;
    let user = repo::insert(
        &mut tx,
        NewUser {
            id: Uuid::now_v7(),
            email: &email,
            name: &name,
            password_hash: &hash,
            is_tenant_admin: false,
        },
    )
    .await
    .map_err(conflict_on_email)?;
    audit(
        &mut tx,
        &ctx,
        "user.create",
        "user",
        user.id,
        json!({ "email": user.email }),
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(user)))
}

fn conflict_on_email(e: sqlx::Error) -> AppError {
    if crate::db::is_unique_violation(&e) {
        AppError::Conflict("email already in use")
    } else {
        e.into()
    }
}

pub async fn get(Tenant(ctx): Tenant, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<UserOut>> {
    repo::get(ctx.db(), id).await?.map(Json).ok_or(AppError::NotFound)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub limit: Option<i64>,
    pub after: Option<Uuid>,
}

pub async fn list(Tenant(ctx): Tenant, ApiQuery(q): ApiQuery<ListQuery>) -> AppResult<Json<Page<UserOut>>> {
    let limit = page_limit(q.limit)?;
    let items = repo::list(ctx.db(), q.after.unwrap_or(Uuid::nil()), limit).await?;
    Ok(Json(page(items, limit, |u| u.id)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchUser {
    pub name: Option<String>,
    pub email: Option<String>,
    pub password: Option<String>,
    pub current_password: Option<String>,
    pub is_active: Option<bool>,
}

/// Regras:
/// - o próprio usuário altera `name` sem feature; a própria senha exige `current_password`;
/// - `email` e `is_active` exigem `users.update`;
/// - admin de tenant só é alterado por usuário de plataforma (o próprio admin
///   pode trocar nome e senha);
/// - não é possível desativar o último admin ativo.
pub async fn patch(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PatchUser>,
) -> AppResult<Json<UserOut>> {
    let is_self = !ctx.is_platform() && id == ctx.actor_id();
    let can_update = ctx.can(Feature::UsersUpdate);
    let privileged = b.email.is_some() || b.is_active.is_some();
    if (privileged || !is_self) && !can_update {
        return Err(AppError::Forbidden("missing feature users.update"));
    }
    if b.name.is_none() && b.email.is_none() && b.password.is_none() && b.is_active.is_none() {
        return Err(AppError::invalid("body", "no fields to update"));
    }
    if b.current_password.is_some() && b.password.is_none() {
        return Err(AppError::invalid(
            "current_password",
            "only allowed together with password",
        ));
    }

    let mut v = Validator::default();
    let name = b.name.as_deref().map(|n| v.name("name", n));
    let email = b.email.as_deref().map(|e| v.email("email", e));
    if let Some(p) = &b.password {
        v.password("password", p);
        if is_self && b.current_password.is_none() {
            v.err("current_password", "required to change your own password");
        }
    }
    v.finish()?;

    if is_self && let (Some(_), Some(current)) = (&b.password, b.current_password.clone()) {
        let stored = repo::password_hash(ctx.db(), id).await?;
        if !st.hasher.verify(current, stored).await? {
            return Err(AppError::Forbidden("current password is invalid"));
        }
    }
    let hash = match b.password {
        Some(p) => Some(st.hasher.hash(p).await?),
        None => None,
    };

    let allow_admin = ctx.is_platform() || (is_self && !privileged);
    let patch = UserPatch {
        name: name.as_deref(),
        email: email.as_deref(),
        password_hash: hash.as_deref(),
        is_active: b.is_active,
    };
    let mut tx = ctx.db().begin().await?;
    let Some(user) = repo::update(&mut tx, id, &patch, allow_admin)
        .await
        .map_err(conflict_on_email)?
    else {
        return Err(match repo::is_admin(&mut *tx, id).await? {
            Some(_) => AppError::Forbidden("only platform users can change a tenant admin"),
            None => AppError::NotFound,
        });
    };
    if user.is_tenant_admin && b.is_active == Some(false) && !repo::lock_and_any_active_admin(&mut tx).await? {
        return Err(AppError::Conflict("cannot deactivate the last active tenant admin"));
    }
    if hash.is_some() || b.is_active == Some(false) {
        repo::revoke_sessions(&mut tx, id).await?;
    }
    let mut fields = Vec::new();
    for (set, f) in [
        (patch.name.is_some(), "name"),
        (patch.email.is_some(), "email"),
        (hash.is_some(), "password"),
        (b.is_active.is_some(), "is_active"),
    ] {
        if set {
            fields.push(f);
        }
    }
    audit(
        &mut tx,
        &ctx,
        "user.update",
        "user",
        id,
        json!({ "fields": fields, "is_active": b.is_active }),
    )
    .await?;
    tx.commit().await?;
    st.publish(Invalidation::TenantUser {
        tid: ctx.tenant.id,
        uid: id,
    })
    .await;
    Ok(Json(user))
}

pub async fn delete(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<StatusCode> {
    let mut tx = ctx.db().begin().await?;
    let Some(deleted) = repo::delete(&mut tx, id, ctx.is_platform()).await? else {
        return Err(match repo::is_admin(&mut *tx, id).await? {
            Some(_) => AppError::Forbidden("only platform users can delete a tenant admin"),
            None => AppError::NotFound,
        });
    };
    if deleted.was_active_admin && !repo::lock_and_any_active_admin(&mut tx).await? {
        return Err(AppError::Conflict("cannot delete the last active tenant admin"));
    }
    audit(&mut tx, &ctx, "user.delete", "user", id, json!({})).await?;
    tx.commit().await?;
    st.publish(Invalidation::TenantUser {
        tid: ctx.tenant.id,
        uid: id,
    })
    .await;
    Ok(StatusCode::NO_CONTENT)
}
