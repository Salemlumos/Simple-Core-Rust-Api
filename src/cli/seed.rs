//! `api seed`: cria o owner e a primeira base. Idempotente e seguro para
//! execuções simultâneas (advisory lock global). Nunca altera a senha,
//! nunca cria outro owner e nunca recria um banco existente.

use anyhow::{Context, bail};
use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use crate::config::{DbConfig, Secret, env_req};
use crate::db::{self, is_valid_slug, tenant_db_name};
use crate::tenancy::provision::{self, Failpoint, Outcome, no_failpoint};
use crate::users::model::Validator;

#[derive(Debug, Clone)]
pub struct SeedParams {
    pub owner_email: String,
    pub owner_name: String,
    pub owner_password: Secret,
    pub tenant_slug: String,
    pub tenant_name: String,
}

impl SeedParams {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            owner_email: env_req("SEED_OWNER_EMAIL")?,
            owner_name: env_req("SEED_OWNER_NAME")?,
            owner_password: Secret::new(env_req("SEED_OWNER_PASSWORD")?),
            tenant_slug: env_req("SEED_TENANT_SLUG")?,
            tenant_name: env_req("SEED_TENANT_NAME")?,
        })
    }

    fn validate(&self) -> anyhow::Result<(String, String, String)> {
        let mut v = Validator::default();
        let email = v.email("SEED_OWNER_EMAIL", &self.owner_email);
        let owner = v.name("SEED_OWNER_NAME", &self.owner_name);
        let tenant = v.name("SEED_TENANT_NAME", &self.tenant_name);
        v.password("SEED_OWNER_PASSWORD", self.owner_password.expose());
        if let Err(crate::error::AppError::Validation(errs)) = v.finish() {
            let fields: Vec<_> = errs.iter().map(|e| format!("{}: {}", e.field, e.message)).collect();
            bail!("parâmetros de seed inválidos: {}", fields.join("; "));
        }
        if self.owner_password.expose().chars().count() < 16 {
            bail!("SEED_OWNER_PASSWORD precisa ter pelo menos 16 caracteres");
        }
        if !is_valid_slug(&self.tenant_slug) {
            bail!("SEED_TENANT_SLUG inválido");
        }
        Ok((email, owner, tenant))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct SeedReport {
    pub owner_created: bool,
    pub tenant_created: bool,
    pub tenant_id: Uuid,
    pub provisioning: Outcome,
}

pub async fn run(db: &DbConfig) -> anyhow::Result<()> {
    let params = SeedParams::from_env()?;
    let r = run_with(db, &params, &no_failpoint).await?;
    tracing::info!(
        owner_created = r.owner_created,
        tenant_created = r.tenant_created,
        tenant = %r.tenant_id,
        provisioning = ?r.provisioning,
        "seed concluído"
    );
    if let Outcome::Failed(e) = r.provisioning {
        bail!("provisionamento da primeira base falhou: {e}");
    }
    Ok(())
}

pub async fn run_with(db: &DbConfig, p: &SeedParams, fp: Failpoint<'_>) -> anyhow::Result<SeedReport> {
    let (email, owner_name, tenant_name) = p.validate()?;
    let mut conn = db::connect("control", &db.migrator(&db.control_db)?, db.connect_retry).await?;
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('simple-core:seed', 0))")
        .execute(&mut conn)
        .await?;
    let res = seed_locked(db, &mut conn, p, &email, &owner_name, &tenant_name, fp).await;
    let _ = sqlx::query("SELECT pg_advisory_unlock(hashtextextended('simple-core:seed', 0))")
        .execute(&mut conn)
        .await;
    let _ = conn.close().await;
    res
}

async fn seed_locked(
    db: &DbConfig,
    conn: &mut PgConnection,
    p: &SeedParams,
    email: &str,
    owner_name: &str,
    tenant_name: &str,
    fp: Failpoint<'_>,
) -> anyhow::Result<SeedReport> {
    // 1. Owner.
    let owner: Option<(Uuid, String)> = sqlx::query_as("SELECT id, email FROM platform_users WHERE role = 'owner'")
        .fetch_optional(&mut *conn)
        .await?;
    let (owner_id, owner_created) = match owner {
        Some((id, existing)) => {
            if existing != email {
                tracing::warn!("já existe um owner com outro e-mail; nada foi alterado");
            }
            (id, false)
        }
        None => {
            let taken: Option<String> = sqlx::query_scalar("SELECT role FROM platform_users WHERE email = $1")
                .bind(email)
                .fetch_optional(&mut *conn)
                .await?;
            if taken.is_some() {
                bail!("o e-mail do owner já pertence a um superusuário; ele não será promovido");
            }
            let password = p.owner_password.expose().to_owned();
            let hash = tokio::task::spawn_blocking(move || crate::password::hash_blocking(&password))
                .await
                .context("tarefa de hash falhou")??;
            let id = Uuid::now_v7();
            let mut tx = conn.begin().await?;
            sqlx::query(
                "INSERT INTO platform_users (id, email, name, password_hash, role) VALUES ($1, $2, $3, $4, 'owner')",
            )
            .bind(id)
            .bind(email)
            .bind(owner_name)
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO platform_audit (id, actor_id, action, target_type, target_id, payload)
                 VALUES ($1, $2, 'owner.seed', 'platform_user', $3, '{}'::jsonb)",
            )
            .bind(Uuid::now_v7())
            .bind(id)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::info!(owner = %id, "owner criado");
            (id, true)
        }
    };

    // 2. Primeiro tenant.
    let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM tenants WHERE slug = $1")
        .bind(&p.tenant_slug)
        .fetch_optional(&mut *conn)
        .await?;
    let (tenant_id, tenant_created) = match existing {
        Some(id) => (id, false),
        None => {
            let id = Uuid::now_v7();
            let mut tx = conn.begin().await?;
            sqlx::query(
                "INSERT INTO tenants (id, slug, name, db_name, status, created_by)
                 VALUES ($1, $2, $3, $4, 'provisioning', $5)",
            )
            .bind(id)
            .bind(&p.tenant_slug)
            .bind(tenant_name)
            .bind(tenant_db_name(id))
            .bind(owner_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO platform_audit (id, actor_id, action, target_type, target_id, payload)
                 VALUES ($1, $2, 'tenant.seed', 'tenant', $3, jsonb_build_object('slug', $4::text))",
            )
            .bind(Uuid::now_v7())
            .bind(owner_id)
            .bind(id.to_string())
            .bind(&p.tenant_slug)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            (id, true)
        }
    };

    // 3. Provisiona ou retoma (no-op se já ativo). Lock da linha: espera um
    //    worker que esteja com o mesmo tenant.
    let mut tx = conn.begin().await?;
    sqlx::query("SELECT id FROM tenants WHERE id = $1 FOR UPDATE")
        .bind(tenant_id)
        .execute(&mut *tx)
        .await?;
    let provisioning = provision::provision(db, &mut tx, tenant_id, fp).await?;
    tx.commit().await?;

    Ok(SeedReport {
        owner_created,
        tenant_created,
        tenant_id,
        provisioning,
    })
}
