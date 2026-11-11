//! Catálogo (somente leitura), perfis, perfis de usuário e exceções.
//!
//! Quem não tem acesso total só concede features que possui. Usuário comum
//! não altera os próprios perfis/exceções, e admins só são alterados por
//! usuários de plataforma.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use super::catalog::{Feature, FeatureSet};
use super::repository::{self as repo, NewOverride, OverrideOut, ProfileOut, ProfileRef, audit};
use super::resolver;
use crate::auth::middleware::Tenant;
use crate::error::{ApiJson, ApiPath, ApiQuery, AppError, AppResult};
use crate::openapi::Problem;
use crate::state::{AppState, Invalidation};
use crate::tenancy::middleware::TenantCtx;
use crate::users::handlers::ListQuery;
use crate::users::model::{Page, Validator, page, page_limit};
use crate::users::repository as users_repo;

const MAX_ITEMS: usize = 100;

/// Catálogo de módulos e features (somente leitura).
#[utoipa::path(get, path = "/v1/modules", tag = "acesso",
    responses((status = 200, description = "Módulos com suas features", body = Vec<repo::ModuleOut>)))]
pub async fn modules(Tenant(ctx): Tenant) -> AppResult<Json<Vec<repo::ModuleOut>>> {
    Ok(Json(repo::modules(ctx.db()).await?))
}

/// Lista os perfis do tenant.
#[utoipa::path(get, path = "/v1/profiles", tag = "acesso: perfis",
    params(ListQuery),
    responses((status = 200, description = "Página de perfis", body = Page<ProfileOut>)))]
pub async fn list_profiles(Tenant(ctx): Tenant, ApiQuery(q): ApiQuery<ListQuery>) -> AppResult<Json<Page<ProfileOut>>> {
    let limit = page_limit(q.limit)?;
    let items = repo::list_profiles(ctx.db(), q.after.unwrap_or(Uuid::nil()), limit).await?;
    Ok(Json(page(items, limit, |p| p.id)))
}

/// Busca um perfil pelo id.
#[utoipa::path(get, path = "/v1/profiles/{id}", tag = "acesso: perfis",
    params(("id" = Uuid, Path, description = "Id do perfil")),
    responses((status = 200, description = "Perfil", body = ProfileOut)))]
pub async fn get_profile(Tenant(ctx): Tenant, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<ProfileOut>> {
    repo::get_profile(ctx.db(), id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

/// Valida códigos de feature contra o catálogo e devolve o conjunto.
fn parse_features(codes: &[String]) -> AppResult<(Vec<String>, FeatureSet)> {
    if codes.len() > MAX_ITEMS {
        return Err(AppError::invalid("features", "too many features"));
    }
    let mut set = FeatureSet::empty();
    let mut out = BTreeSet::new();
    for c in codes {
        let f = Feature::from_code(c).ok_or(AppError::invalid("features", "unknown feature code"))?;
        set.insert(f);
        out.insert(f.code().to_owned());
    }
    Ok((out.into_iter().collect(), set))
}

fn ensure_grantable(ctx: &TenantCtx, set: FeatureSet) -> AppResult<()> {
    if set.is_subset_of(ctx.grantable()) {
        Ok(())
    } else {
        Err(AppError::Forbidden("cannot grant features you do not have"))
    }
}

fn validate_description(v: &mut Validator, d: &str) {
    if d.chars().count() > 500 {
        v.err("description", "must have at most 500 characters");
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProfile {
    pub name: String,
    pub description: Option<String>,
    /// Códigos de features (veja `GET /v1/modules`).
    #[serde(default)]
    pub features: Vec<String>,
}

/// Cria um perfil. Só é possível incluir features que o próprio usuário tem.
#[utoipa::path(post, path = "/v1/profiles", tag = "acesso: perfis",
    request_body = CreateProfile,
    responses(
        (status = 201, description = "Perfil criado", body = ProfileOut),
        (status = 409, description = "Nome de perfil já usado", body = Problem, content_type = "application/problem+json"),
    ))]
pub async fn create_profile(
    Tenant(ctx): Tenant,
    ApiJson(b): ApiJson<CreateProfile>,
) -> AppResult<(StatusCode, Json<ProfileOut>)> {
    let mut v = Validator::default();
    let name = v.name("name", &b.name);
    if let Some(d) = &b.description {
        validate_description(&mut v, d);
    }
    v.finish()?;
    let (codes, set) = parse_features(&b.features)?;
    ensure_grantable(&ctx, set)?;

    let id = Uuid::now_v7();
    let mut tx = ctx.db().begin().await?;
    repo::insert_profile(&mut tx, id, &name, b.description.as_deref().filter(|d| !d.is_empty()))
        .await
        .map_err(name_conflict)?;
    repo::replace_profile_features(&mut tx, id, &codes).await?;
    audit(
        &mut tx,
        &ctx,
        "profile.create",
        "profile",
        id,
        json!({ "name": name, "features": codes }),
    )
    .await?;
    let out = repo::get_profile(&mut *tx, id).await?.ok_or(AppError::NotFound)?;
    // Perfil novo ainda não tem usuários: nada a invalidar.
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(out)))
}

fn name_conflict(e: sqlx::Error) -> AppError {
    if crate::db::is_unique_violation(&e) {
        AppError::Conflict("profile name already in use")
    } else {
        e.into()
    }
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchProfile {
    pub name: Option<String>,
    pub description: Option<String>,
}

/// Altera nome ou descrição de um perfil.
#[utoipa::path(patch, path = "/v1/profiles/{id}", tag = "acesso: perfis",
    params(("id" = Uuid, Path, description = "Id do perfil")),
    request_body = PatchProfile,
    responses(
        (status = 200, description = "Perfil atualizado", body = ProfileOut),
        (status = 409, description = "Nome de perfil já usado", body = Problem, content_type = "application/problem+json"),
    ))]
pub async fn patch_profile(
    Tenant(ctx): Tenant,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PatchProfile>,
) -> AppResult<Json<ProfileOut>> {
    let mut v = Validator::default();
    let name = b.name.as_deref().map(|n| v.name("name", n));
    if let Some(d) = &b.description {
        validate_description(&mut v, d);
    }
    if b.name.is_none() && b.description.is_none() {
        v.err("body", "no fields to update");
    }
    v.finish()?;
    let mut tx = ctx.db().begin().await?;
    if !repo::update_profile(&mut tx, id, name.as_deref(), b.description.as_deref())
        .await
        .map_err(name_conflict)?
    {
        return Err(AppError::NotFound);
    }
    audit(
        &mut tx,
        &ctx,
        "profile.update",
        "profile",
        id,
        json!({ "name": name, "description": b.description }),
    )
    .await?;
    let out = repo::get_profile(&mut *tx, id).await?.ok_or(AppError::NotFound)?;
    tx.commit().await?;
    Ok(Json(out))
}

/// Remove um perfil (os usuários perdem as features dele).
#[utoipa::path(delete, path = "/v1/profiles/{id}", tag = "acesso: perfis",
    params(("id" = Uuid, Path, description = "Id do perfil")),
    responses((status = 204, description = "Perfil removido")))]
pub async fn delete_profile(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiPath(id): ApiPath<Uuid>,
) -> AppResult<StatusCode> {
    let mut tx = ctx.db().begin().await?;
    if !repo::delete_profile(&mut tx, id).await? {
        return Err(AppError::NotFound);
    }
    audit(&mut tx, &ctx, "profile.delete", "profile", id, json!({})).await?;
    tx.commit().await?;
    st.publish(Invalidation::TenantAll { tid: ctx.tenant.id }).await;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PutFeatures {
    pub features: Vec<String>,
}

/// Substitui as features de um perfil.
///
/// Só é possível conceder features que o próprio usuário tem.
#[utoipa::path(put, path = "/v1/profiles/{id}/features", tag = "acesso: perfis",
    params(("id" = Uuid, Path, description = "Id do perfil")),
    request_body = PutFeatures,
    responses((status = 200, description = "Perfil atualizado", body = ProfileOut)))]
pub async fn put_profile_features(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PutFeatures>,
) -> AppResult<Json<ProfileOut>> {
    let (codes, set) = parse_features(&b.features)?;
    let mut tx = ctx.db().begin().await?;
    if !repo::lock_profile(&mut tx, id).await? {
        return Err(AppError::NotFound);
    }
    let old = FeatureSet::from_codes(repo::profile_feature_codes(&mut tx, id).await?);
    // Só o que está sendo adicionado precisa ser concedível.
    ensure_grantable(&ctx, set.difference(old))?;
    repo::replace_profile_features(&mut tx, id, &codes).await?;
    audit(
        &mut tx,
        &ctx,
        "profile.features.replace",
        "profile",
        id,
        json!({ "features": codes }),
    )
    .await?;
    let out = repo::get_profile(&mut *tx, id).await?.ok_or(AppError::NotFound)?;
    tx.commit().await?;
    st.publish(Invalidation::TenantAll { tid: ctx.tenant.id }).await;
    Ok(Json(out))
}

/// Verifica se o ator pode alterar perfis/exceções do usuário alvo.
async fn guard_manage_user(ctx: &TenantCtx, conn: &mut PgConnection, target: Uuid) -> AppResult<()> {
    let is_self = !ctx.is_platform() && target == ctx.actor_id();
    if is_self && !ctx.full_access() {
        return Err(AppError::Forbidden("cannot change your own access"));
    }
    let is_admin = users_repo::is_admin(&mut *conn, target)
        .await?
        .ok_or(AppError::NotFound)?;
    if is_admin && !ctx.is_platform() && !is_self {
        return Err(AppError::Forbidden("only platform users can change a tenant admin"));
    }
    Ok(())
}

#[derive(Serialize, ToSchema)]
pub struct UserAccess {
    pub user_id: Uuid,
    pub is_tenant_admin: bool,
    pub is_active: bool,
    pub profiles: Vec<ProfileRef>,
    pub overrides: Vec<OverrideOut>,
    /// Resultado final: (perfis + allows) - denys. Admins recebem todas.
    #[schema(value_type = Vec<String>)]
    pub effective_features: Vec<&'static str>,
}

/// Perfis, exceções e features efetivas de um usuário.
#[utoipa::path(get, path = "/v1/users/{id}/access", tag = "acesso: usuários",
    params(("id" = Uuid, Path, description = "Id do usuário")),
    responses((status = 200, description = "Acesso do usuário", body = UserAccess)))]
pub async fn user_access(Tenant(ctx): Tenant, ApiPath(id): ApiPath<Uuid>) -> AppResult<Json<UserAccess>> {
    let user = users_repo::get(ctx.db(), id).await?.ok_or(AppError::NotFound)?;
    let (profiles, overrides, features) = tokio::try_join!(
        repo::user_profiles(ctx.db(), id),
        repo::user_overrides(ctx.db(), id),
        resolver::effective_features(ctx.db(), id),
    )?;
    let effective = if !user.is_active {
        FeatureSet::empty()
    } else if user.is_tenant_admin {
        FeatureSet::all()
    } else {
        features
    };
    Ok(Json(UserAccess {
        user_id: id,
        is_tenant_admin: user.is_tenant_admin,
        is_active: user.is_active,
        profiles,
        overrides,
        effective_features: effective.codes(),
    }))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PutProfiles {
    pub profile_ids: Vec<Uuid>,
}

/// Substitui os perfis de um usuário.
///
/// Ninguém altera o próprio acesso; admins só são alterados por usuários de plataforma.
#[utoipa::path(put, path = "/v1/users/{id}/profiles", tag = "acesso: usuários",
    params(("id" = Uuid, Path, description = "Id do usuário")),
    request_body = PutProfiles,
    responses((status = 200, description = "Perfis atuais do usuário", body = Vec<ProfileRef>)))]
pub async fn put_user_profiles(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(b): ApiJson<PutProfiles>,
) -> AppResult<Json<Vec<ProfileRef>>> {
    if b.profile_ids.len() > MAX_ITEMS {
        return Err(AppError::invalid("profile_ids", "too many profiles"));
    }
    let ids: Vec<Uuid> = b
        .profile_ids
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut tx = ctx.db().begin().await?;
    guard_manage_user(&ctx, &mut tx, id).await?;
    let added = repo::replace_user_profiles(&mut tx, id, &ids).await.map_err(|e| {
        if crate::db::is_fk_violation(&e) {
            AppError::invalid("profile_ids", "unknown profile")
        } else {
            e.into()
        }
    })?;
    if !added.is_empty() {
        let granted = FeatureSet::from_codes(repo::features_of_profiles(&mut tx, &added).await?);
        ensure_grantable(&ctx, granted)?;
    }
    audit(
        &mut tx,
        &ctx,
        "user.profiles.replace",
        "user",
        id,
        json!({ "profile_ids": ids }),
    )
    .await?;
    let out = repo::user_profiles(&mut *tx, id).await?;
    tx.commit().await?;
    st.publish(Invalidation::TenantUser {
        tid: ctx.tenant.id,
        uid: id,
    })
    .await;
    Ok(Json(out))
}

#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PutOverride {
    /// `allow` ou `deny`.
    #[schema(example = "deny")]
    pub effect: String,
    /// Motivo (1 a 500 caracteres), gravado na auditoria.
    pub reason: String,
    /// Opcional; precisa estar no futuro.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

/// Cria ou substitui uma exceção (allow/deny) de uma feature para o usuário.
///
/// Deny sempre vence perfis e allows. Conceder (allow) exige ter a feature.
#[utoipa::path(put, path = "/v1/users/{id}/overrides/{feature_code}", tag = "acesso: usuários",
    params(("id" = Uuid, Path, description = "Id do usuário"), ("feature_code" = String, Path, description = "Código da feature, por exemplo `users.read`")),
    request_body = PutOverride,
    responses((status = 200, description = "Exceção gravada", body = OverrideOut)))]
pub async fn put_override(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiPath((id, code)): ApiPath<(Uuid, String)>,
    ApiJson(b): ApiJson<PutOverride>,
) -> AppResult<Json<OverrideOut>> {
    let feature = Feature::from_code(&code).ok_or(AppError::NotFound)?;
    let mut v = Validator::default();
    if b.effect != "allow" && b.effect != "deny" {
        v.err("effect", "must be allow or deny");
    }
    let reason = b.reason.trim();
    if reason.is_empty() || reason.chars().count() > 500 {
        v.err("reason", "required, at most 500 characters");
    }
    if b.expires_at.is_some_and(|e| e <= OffsetDateTime::now_utc()) {
        v.err("expires_at", "must be in the future");
    }
    v.finish()?;
    if b.effect == "allow" && !ctx.grantable().contains(feature) {
        return Err(AppError::Forbidden("cannot grant features you do not have"));
    }
    let mut tx = ctx.db().begin().await?;
    guard_manage_user(&ctx, &mut tx, id).await?;
    let out = repo::upsert_override(
        &mut tx,
        NewOverride {
            user: id,
            feature: feature.code(),
            effect: &b.effect,
            reason,
            expires_at: b.expires_at,
            created_by: ctx.actor_id(),
            created_by_type: ctx.actor_type(),
        },
    )
    .await?
    .ok_or(AppError::NotFound)?;
    audit(
        &mut tx,
        &ctx,
        "user.override.put",
        "user",
        id,
        json!({ "feature": feature.code(), "effect": b.effect, "reason": reason, "expires_at": out.expires_at.map(|e| e.unix_timestamp()) }),
    )
    .await?;
    tx.commit().await?;
    st.publish(Invalidation::TenantUser {
        tid: ctx.tenant.id,
        uid: id,
    })
    .await;
    Ok(Json(out))
}

/// Remove a exceção de uma feature para o usuário.
#[utoipa::path(delete, path = "/v1/users/{id}/overrides/{feature_code}", tag = "acesso: usuários",
    params(("id" = Uuid, Path, description = "Id do usuário"), ("feature_code" = String, Path, description = "Código da feature, por exemplo `users.read`")),
    responses((status = 204, description = "Exceção removida")))]
pub async fn delete_override(
    State(st): State<AppState>,
    Tenant(ctx): Tenant,
    ApiPath((id, code)): ApiPath<(Uuid, String)>,
) -> AppResult<StatusCode> {
    let feature = Feature::from_code(&code).ok_or(AppError::NotFound)?;
    let mut tx = ctx.db().begin().await?;
    guard_manage_user(&ctx, &mut tx, id).await?;
    let effect = repo::delete_override(&mut tx, id, feature.code())
        .await?
        .ok_or(AppError::NotFound)?;
    // Remover um deny pode devolver a feature: equivale a concedê-la.
    if effect == "deny" && !ctx.grantable().contains(feature) {
        return Err(AppError::Forbidden("cannot grant features you do not have"));
    }
    audit(
        &mut tx,
        &ctx,
        "user.override.delete",
        "user",
        id,
        json!({ "feature": feature.code(), "effect": effect }),
    )
    .await?;
    tx.commit().await?;
    st.publish(Invalidation::TenantUser {
        tid: ctx.tenant.id,
        uid: id,
    })
    .await;
    Ok(StatusCode::NO_CONTENT)
}
