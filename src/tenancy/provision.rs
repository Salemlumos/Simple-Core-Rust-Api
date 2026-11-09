//! Provisionamento retomável de um banco de tenant, usado pelo `seed` e pelo
//! `worker`. Como `CREATE DATABASE` não roda em transação, cada etapa é
//! idempotente e pode ser repetida após uma falha ou interrupção.
//!
//! 1. advisory lock do tenant na conexão dedicada do `control`;
//! 2. revalida `db_name`;
//! 3. cria o banco se não existir (`TEMPLATE template0`, `UTF8`);
//! 4. restrições de `CONNECT` e privilégios;
//! 5. migrations do tenant;
//! 6. sincroniza o catálogo;
//! 7. marca `active` (ou `failed` com `last_error` sem segredos).

use sqlx::{AssertSqlSafe, Connection, PgConnection};
use uuid::Uuid;

use crate::config::DbConfig;
use crate::db::{self, TENANT_MIGRATOR, is_tenant_db_name, quote_ident};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    CreateDatabase,
    Privileges,
    Migrations,
    Catalog,
    Activate,
}

impl Step {
    pub const ALL: [Step; 5] = [
        Step::CreateDatabase,
        Step::Privileges,
        Step::Migrations,
        Step::Catalog,
        Step::Activate,
    ];
}

/// Ponto de falha injetável (testes de interrupção). Em produção, [`no_failpoint`].
pub type Failpoint<'a> = &'a (dyn Fn(Step) -> anyhow::Result<()> + Send + Sync);

pub fn no_failpoint(_: Step) -> anyhow::Result<()> {
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Activated,
    AlreadyActive,
    Skipped(String),
    Failed(String),
}

/// `ctl` é uma conexão dedicada ao `control` (role `app_migrator`); pode
/// estar dentro de uma transação que segura o lock da linha do tenant.
pub async fn provision(db: &DbConfig, ctl: &mut PgConnection, id: Uuid, fp: Failpoint<'_>) -> anyhow::Result<Outcome> {
    db::lock_tenant(ctl, id).await?;
    let res = provision_locked(db, ctl, id, fp).await;
    let unlock = db::unlock_tenant(ctl, id).await;
    let out = res?;
    unlock?;
    Ok(out)
}

async fn provision_locked(
    db: &DbConfig,
    ctl: &mut PgConnection,
    id: Uuid,
    fp: Failpoint<'_>,
) -> anyhow::Result<Outcome> {
    let Some(row) = sqlx::query!("SELECT db_name, status FROM tenants WHERE id = $1", id)
        .fetch_optional(&mut *ctl)
        .await?
    else {
        anyhow::bail!("tenant {id} não existe");
    };
    match row.status.as_str() {
        "active" => return Ok(Outcome::AlreadyActive),
        "provisioning" | "failed" => {}
        other => return Ok(Outcome::Skipped(other.to_owned())),
    }
    let started = std::time::Instant::now();
    match run_steps(db, &row.db_name, fp).await {
        Ok(()) => {
            sqlx::query!(
                "UPDATE tenants SET status = 'active', last_error = NULL, updated_at = now() WHERE id = $1",
                id
            )
            .execute(&mut *ctl)
            .await?;
            metrics::counter!("tenant_provisioning_total", "result" => "ok").increment(1);
            metrics::histogram!("tenant_provisioning_duration_seconds").record(started.elapsed().as_secs_f64());
            tracing::info!(tenant = %id, "tenant provisionado");
            Ok(Outcome::Activated)
        }
        Err(e) => {
            let msg = sanitize(&e);
            sqlx::query!(
                "UPDATE tenants SET status = 'failed', last_error = $2, updated_at = now() WHERE id = $1",
                id,
                msg
            )
            .execute(&mut *ctl)
            .await?;
            metrics::counter!("tenant_provisioning_total", "result" => "failed").increment(1);
            tracing::warn!(tenant = %id, error = %msg, "falha no provisionamento");
            Ok(Outcome::Failed(msg))
        }
    }
}

/// Mensagem curta e sem segredos (erros do sqlx não contêm credenciais).
fn sanitize(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    s.chars().take(500).collect()
}

async fn run_steps(db: &DbConfig, db_name: &str, fp: Failpoint<'_>) -> anyhow::Result<()> {
    anyhow::ensure!(is_tenant_db_name(db_name), "db_name inválido");
    let ident = quote_ident(db_name);
    let runtime = quote_ident(&db.runtime_user);

    fp(Step::CreateDatabase)?;
    let mut ctl = db::connect("control", &db.migrator(&db.control_db)?, db.connect_retry).await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
        .bind(db_name)
        .fetch_one(&mut ctl)
        .await?;
    if !exists {
        let sql = format!("CREATE DATABASE {ident} TEMPLATE template0 ENCODING 'UTF8'");
        if let Err(e) = sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut ctl).await
            && db::pg_code(&e).as_deref() != Some("42P04")
        {
            return Err(e.into());
        }
    }

    fp(Step::Privileges)?;
    let sql = format!("REVOKE ALL ON DATABASE {ident} FROM PUBLIC; GRANT CONNECT ON DATABASE {ident} TO {runtime};");
    sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut ctl).await?;
    ctl.close().await?;

    let mut tconn = db::connect("tenant", &db.migrator(db_name)?, db.connect_retry).await?;
    let sql = format!("REVOKE ALL ON SCHEMA public FROM PUBLIC; GRANT USAGE ON SCHEMA public TO {runtime};");
    sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut tconn).await?;

    fp(Step::Migrations)?;
    TENANT_MIGRATOR.run(&mut tconn).await?;
    apply_tenant_grants(&mut tconn, &db.runtime_user).await?;

    fp(Step::Catalog)?;
    crate::access::catalog::sync(&mut tconn).await?;
    tconn.close().await?;

    fp(Step::Activate)?;
    Ok(())
}

/// Privilégios de `app_runtime` num banco de tenant: só DML; `access_audit`
/// só `SELECT`/`INSERT`; catálogo somente leitura. Atômico e idempotente.
pub async fn apply_tenant_grants(conn: &mut PgConnection, runtime_user: &str) -> anyhow::Result<()> {
    let rt = quote_ident(runtime_user);
    let sql = format!(
        "REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {rt};
         REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM {rt};
         GRANT SELECT ON modules, features TO {rt};
         GRANT SELECT, INSERT, UPDATE, DELETE ON users, profiles, profile_features, user_profiles,
           user_feature_overrides, refresh_tokens TO {rt};
         GRANT SELECT, INSERT ON access_audit TO {rt};"
    );
    let mut tx = conn.begin().await?;
    sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Privilégios de `app_runtime` no `control`, com UPDATE restrito por coluna.
pub async fn apply_control_grants(conn: &mut PgConnection, runtime_user: &str) -> anyhow::Result<()> {
    let rt = quote_ident(runtime_user);
    let sql = format!(
        "GRANT USAGE ON SCHEMA public TO {rt};
         REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {rt};
         REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM {rt};
         GRANT SELECT, INSERT ON tenants TO {rt};
         GRANT UPDATE (name, status, last_error, updated_at) ON tenants TO {rt};
         GRANT SELECT, INSERT, DELETE ON platform_users TO {rt};
         GRANT UPDATE (name, password_hash, is_active, updated_at) ON platform_users TO {rt};
         GRANT SELECT, INSERT, UPDATE, DELETE ON platform_refresh_tokens TO {rt};
         GRANT SELECT, INSERT ON platform_audit TO {rt};"
    );
    let mut tx = conn.begin().await?;
    sqlx::raw_sql(AssertSqlSafe(sql)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
