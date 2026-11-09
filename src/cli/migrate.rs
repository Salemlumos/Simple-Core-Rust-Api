//! `api migrate`: migra o `control` e todos os bancos de tenant já criados,
//! sincroniza o catálogo, com concorrência limitada. Um tenant com falha não
//! interrompe os demais; o comando retorna erro ao final.

use futures::StreamExt;
use uuid::Uuid;

use crate::config::{DbConfig, env_parse};
use crate::db::{self, CONTROL_MIGRATOR, TENANT_MIGRATOR, is_tenant_db_name};
use crate::tenancy::provision::{apply_control_grants, apply_tenant_grants};

pub async fn run(db: &DbConfig) -> anyhow::Result<()> {
    let concurrency = env_parse("MIGRATE_CONCURRENCY", 4usize)?.max(1);
    run_with(db, concurrency).await
}

pub async fn run_with(db: &DbConfig, concurrency: usize) -> anyhow::Result<()> {
    let mut ctl = db::connect("control", &db.migrator(&db.control_db)?, db.connect_retry).await?;
    CONTROL_MIGRATOR.run(&mut ctl).await?;
    apply_control_grants(&mut ctl, &db.runtime_user).await?;
    tracing::info!("control migrado");

    // Só bancos já provisionados; `provisioning`/`failed` ficam com o worker,
    // que aplica as migrations ao retomar.
    let tenants: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, db_name FROM tenants WHERE status IN ('active', 'suspended') ORDER BY id")
            .fetch_all(&mut ctl)
            .await?;
    drop(ctl);

    let total = tenants.len();
    let failures: Vec<(Uuid, anyhow::Error)> = futures::stream::iter(tenants)
        .map(|(id, db_name)| async move { (id, migrate_tenant(db, id, &db_name).await) })
        .buffer_unordered(concurrency)
        .filter_map(|(id, r)| async move { r.err().map(|e| (id, e)) })
        .collect()
        .await;

    for (id, e) in &failures {
        tracing::error!(tenant = %id, error = format!("{e:#}"), "falha ao migrar tenant");
    }
    tracing::info!(total, failed = failures.len(), "migração dos tenants concluída");
    if !failures.is_empty() {
        anyhow::bail!("{} de {total} tenants falharam na migração", failures.len());
    }
    Ok(())
}

/// Migra um tenant sob o mesmo advisory lock do provisionamento.
pub async fn migrate_tenant(db: &DbConfig, id: Uuid, db_name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(is_tenant_db_name(db_name), "db_name inválido");
    let mut lock = db::connect("control", &db.migrator(&db.control_db)?, db.connect_retry).await?;
    db::lock_tenant(&mut lock, id).await?;
    let res = async {
        let mut conn = db::connect("tenant", &db.migrator(db_name)?, db.connect_retry).await?;
        TENANT_MIGRATOR.run(&mut conn).await?;
        apply_tenant_grants(&mut conn, &db.runtime_user).await?;
        crate::access::catalog::sync(&mut conn).await?;
        anyhow::Ok(())
    }
    .await;
    let _ = db::unlock_tenant(&mut lock, id).await;
    res
}
