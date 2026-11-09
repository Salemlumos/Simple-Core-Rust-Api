//! `api worker`: provisiona tenants pendentes. Acordado por `NOTIFY` no
//! `control` e, por segurança, a cada 10 s. Usa `FOR UPDATE SKIP LOCKED` e
//! advisory lock por tenant para nunca processar o mesmo tenant em dobro.

use std::time::Duration;

use sqlx::Connection;
use sqlx::postgres::{PgListener, PgPoolOptions};
use uuid::Uuid;

use crate::config::DbConfig;
use crate::db::{self, CHANNEL_PROVISIONING};
use crate::tenancy::provision::{self, Failpoint, Outcome, no_failpoint};

const POLL: Duration = Duration::from_secs(10);

/// Reivindica e provisiona um tenant pendente. `None` se não há pendências.
pub async fn process_one(db: &DbConfig, fp: Failpoint<'_>) -> anyhow::Result<Option<(Uuid, Outcome)>> {
    let mut conn = db::connect("control", &db.migrator(&db.control_db)?, db.connect_retry).await?;
    let mut tx = conn.begin().await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tenants WHERE status = 'provisioning'
         ORDER BY created_at, id FOR UPDATE SKIP LOCKED LIMIT 1",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = id else {
        return Ok(None);
    };
    let outcome = provision::provision(db, &mut tx, id, fp).await?;
    tx.commit().await?;
    let _ = conn.close().await;
    Ok(Some((id, outcome)))
}

/// Processa até não haver mais pendências.
pub async fn drain(db: &DbConfig, fp: Failpoint<'_>) -> anyhow::Result<usize> {
    let mut n = 0;
    while process_one(db, fp).await?.is_some() {
        n += 1;
    }
    Ok(n)
}

async fn pending(db: &DbConfig) -> anyhow::Result<i64> {
    let mut conn = db::connect("control", &db.migrator(&db.control_db)?, db.connect_retry).await?;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM tenants WHERE status = 'provisioning'")
        .fetch_one(&mut conn)
        .await?;
    let _ = conn.close().await;
    Ok(n)
}

pub async fn run(db: &DbConfig) -> anyhow::Result<()> {
    let listen_pool = PgPoolOptions::new()
        .max_connections(1)
        .min_connections(0)
        .connect_lazy_with(db.migrator(&db.control_db)?);
    let mut listener = db::retry("control", db.connect_retry, || PgListener::connect_with(&listen_pool)).await?;
    listener.listen(CHANNEL_PROVISIONING).await?;
    tracing::info!("worker aguardando tenants pendentes");

    let shutdown = crate::app::shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        match pending(db).await {
            Ok(n) => metrics::gauge!("tenant_provisioning_queue").set(n as f64),
            Err(e) => tracing::warn!(error = format!("{e:#}"), "falha ao medir a fila"),
        }
        if let Err(e) = drain(db, &no_failpoint).await {
            tracing::error!(
                error = format!("{e:#}"),
                "erro no worker; nova tentativa no próximo ciclo"
            );
        }
        tokio::select! {
            () = &mut shutdown => break,
            r = listener.recv() => {
                if let Err(e) = r {
                    tracing::warn!(error = %e, "listener do worker falhou; reconectando");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
            () = tokio::time::sleep(POLL) => {}
        }
    }
    tracing::info!("worker encerrado");
    Ok(())
}
