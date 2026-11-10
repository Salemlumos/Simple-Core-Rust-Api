//! Um `PgPool` lazy por tenant, guardado num `moka` LRU com expiração por
//! inatividade de 10 minutos.
//!
//! O pool fica dentro de um `Arc<TenantPool>`. Quando sai do cache, o `Drop`
//! do último `Arc` fecha o pool numa task separada: requisições que já pegaram
//! o handle terminam normalmente e nada bloqueia o caminho da requisição.

use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use moka::notification::RemovalCause;
use moka::policy::EvictionPolicy;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::registry::TenantInfo;
use crate::access::cache::record;
use crate::config::DbConfig;
use crate::db::is_tenant_db_name;
use crate::error::{AppError, AppResult};

pub struct TenantPool {
    pub tenant_id: Uuid,
    pub pool: PgPool,
}

impl Drop for TenantPool {
    fn drop(&mut self) {
        metrics::gauge!("tenant_pools_open").decrement(1.0);
        let pool = self.pool.clone();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move { pool.close().await });
        }
    }
}

#[derive(Clone)]
pub struct TenantPools {
    cache: Cache<Uuid, Arc<TenantPool>>,
    db: Arc<DbConfig>,
    max_connections: u32,
    acquire_timeout: Duration,
    statement_timeout_ms: u64,
}

impl TenantPools {
    pub fn new(
        db: Arc<DbConfig>,
        max_open: u64,
        max_connections: u32,
        acquire_timeout: Duration,
        statement_timeout_ms: u64,
    ) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_open)
            .eviction_policy(EvictionPolicy::lru())
            .time_to_idle(Duration::from_secs(600))
            .eviction_listener(|_k: Arc<Uuid>, _v: Arc<TenantPool>, cause: RemovalCause| {
                let cause = match cause {
                    RemovalCause::Expired => "idle",
                    RemovalCause::Size => "capacity",
                    RemovalCause::Explicit => "explicit",
                    RemovalCause::Replaced => "replaced",
                };
                metrics::counter!("tenant_pool_evictions_total", "cause" => cause).increment(1);
            })
            .build();
        Self {
            cache,
            db,
            max_connections,
            acquire_timeout,
            statement_timeout_ms,
        }
    }

    /// Pool do tenant da requisição. Nunca existe pool padrão.
    pub async fn get(&self, tenant: &TenantInfo) -> AppResult<Arc<TenantPool>> {
        let mut created = false;
        let res = self
            .cache
            .try_get_with(tenant.id, async {
                created = true;
                self.build(tenant).map(Arc::new)
            })
            .await;
        record("tenant_pool", !created);
        res.map_err(|e| AppError::Internal(anyhow::anyhow!("{e}")))
    }

    fn build(&self, tenant: &TenantInfo) -> anyhow::Result<TenantPool> {
        anyhow::ensure!(
            is_tenant_db_name(&tenant.db_name),
            "db_name inválido para o tenant {}",
            tenant.id
        );
        let opts = self
            .db
            .runtime(&tenant.db_name)?
            .options([("statement_timeout", format!("{}ms", self.statement_timeout_ms))]);
        let pool = PgPoolOptions::new()
            .max_connections(self.max_connections)
            .min_connections(0)
            .idle_timeout(Duration::from_secs(60))
            // Sem ping a cada acquire: medido no README (01 -> 02).
            .test_before_acquire(false)
            .acquire_timeout(self.acquire_timeout)
            .connect_lazy_with(opts);
        metrics::gauge!("tenant_pools_open").increment(1.0);
        Ok(TenantPool {
            tenant_id: tenant.id,
            pool,
        })
    }

    pub async fn remove(&self, tenant: Uuid) {
        self.cache.invalidate(&tenant).await;
    }

    /// Para métricas: conexões abertas e em uso somadas entre os pools.
    pub fn connection_stats(&self) -> (u64, u64, u64) {
        let (mut pools, mut size, mut busy) = (0u64, 0u64, 0u64);
        for (_, p) in self.cache.iter() {
            pools += 1;
            let s = u64::from(p.pool.size());
            size += s;
            busy += s.saturating_sub(p.pool.num_idle() as u64);
        }
        (pools, size, busy)
    }

    pub async fn run_pending_tasks(&self) {
        self.cache.run_pending_tasks().await;
    }
}
