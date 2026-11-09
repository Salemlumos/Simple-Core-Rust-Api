//! Resolução de permissões efetivas:
//! `(features dos perfis ∪ allow válidos) − deny válidos`, numa única query.

use sqlx::PgPool;
use uuid::Uuid;

use super::catalog::FeatureSet;

/// Visão imutável de um usuário de tenant usada na autorização.
#[derive(Debug, Clone)]
pub struct Principal {
    pub is_active: bool,
    pub is_tenant_admin: bool,
    pub features: FeatureSet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformRole {
    Owner,
    Superuser,
}

impl PlatformRole {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "superuser" => Some(Self::Superuser),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct PlatformPrincipal {
    pub role: PlatformRole,
    pub is_active: bool,
}

/// Carrega usuário + permissões efetivas numa única ida ao banco.
pub async fn load_principal(pool: &PgPool, user_id: Uuid) -> Result<Option<Principal>, sqlx::Error> {
    let row = sqlx::query!(
        r#"
        SELECT u.is_active, u.is_tenant_admin,
          ARRAY(
            SELECT f.code
            FROM features f
            WHERE (
              EXISTS (
                SELECT 1
                FROM user_profiles up
                JOIN profile_features pf ON pf.profile_id = up.profile_id
                WHERE up.user_id = $1 AND pf.feature_id = f.id
              )
              OR EXISTS (
                SELECT 1
                FROM user_feature_overrides o
                WHERE o.user_id = $1 AND o.feature_id = f.id AND o.effect = 'allow'
                  AND (o.expires_at IS NULL OR o.expires_at > now())
              )
            )
            AND NOT EXISTS (
              SELECT 1
              FROM user_feature_overrides o
              WHERE o.user_id = $1 AND o.feature_id = f.id AND o.effect = 'deny'
                AND (o.expires_at IS NULL OR o.expires_at > now())
            )
          ) AS "features!"
        FROM users u
        WHERE u.id = $1
        "#,
        user_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|r| Principal {
        is_active: r.is_active,
        is_tenant_admin: r.is_tenant_admin,
        features: FeatureSet::from_codes(&r.features),
    }))
}

/// Permissões efetivas (sem considerar admin), para `GET /users/{id}/access`.
pub async fn effective_features<'e, E>(exec: E, user_id: Uuid) -> Result<FeatureSet, sqlx::Error>
where
    E: sqlx::PgExecutor<'e>,
{
    let codes = sqlx::query_scalar!(
        r#"
        SELECT f.code
        FROM features f
        WHERE (
          EXISTS (
            SELECT 1
            FROM user_profiles up
            JOIN profile_features pf ON pf.profile_id = up.profile_id
            WHERE up.user_id = $1 AND pf.feature_id = f.id
          )
          OR EXISTS (
            SELECT 1
            FROM user_feature_overrides o
            WHERE o.user_id = $1 AND o.feature_id = f.id AND o.effect = 'allow'
              AND (o.expires_at IS NULL OR o.expires_at > now())
          )
        )
        AND NOT EXISTS (
          SELECT 1
          FROM user_feature_overrides o
          WHERE o.user_id = $1 AND o.feature_id = f.id AND o.effect = 'deny'
            AND (o.expires_at IS NULL OR o.expires_at > now())
        )
        "#,
        user_id
    )
    .fetch_all(exec)
    .await?;
    Ok(FeatureSet::from_codes(&codes))
}

pub async fn load_platform_principal(pool: &PgPool, id: Uuid) -> Result<Option<PlatformPrincipal>, sqlx::Error> {
    let row = sqlx::query!("SELECT role, is_active FROM platform_users WHERE id = $1", id)
        .fetch_optional(pool)
        .await?;
    Ok(row.and_then(|r| {
        PlatformRole::parse(&r.role).map(|role| PlatformPrincipal {
            role,
            is_active: r.is_active,
        })
    }))
}
