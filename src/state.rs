//! Estado compartilhado da API e invalidação de caches entre instâncias.

use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::postgres::{PgListener, PgPoolOptions};
use uuid::Uuid;

use crate::access::cache::{AccessCache, PlatformCache};
use crate::auth::jwt::JwtKeys;
use crate::auth::ratelimit::LoginLimiter;
use crate::config::{ApiConfig, DbConfig};
use crate::db::CHANNEL_INVALIDATION;
use crate::password::PasswordHasher;
use crate::tenancy::pools::TenantPools;
use crate::tenancy::registry::TenantRegistry;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

pub struct Inner {
    pub cfg: ApiConfig,
    pub db: Arc<DbConfig>,
    /// Pool do banco `control` (role `app_runtime`).
    pub control: PgPool,
    pub registry: TenantRegistry,
    pub pools: TenantPools,
    pub access: AccessCache,
    pub platform: PlatformCache,
    pub hasher: PasswordHasher,
    pub jwt: JwtKeys,
    pub limiter: LoginLimiter,
}

impl Deref for AppState {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// Mensagem publicada no canal `cache_invalidation` do `control`.
/// `tenant` e `platform_user` também são emitidas por triggers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "k", rename_all = "snake_case")]
pub enum Invalidation {
    Tenant { id: Uuid, slug: String },
    PlatformUser { id: Uuid },
    TenantUser { tid: Uuid, uid: Uuid },
    TenantAll { tid: Uuid },
}

fn control_options(cfg: &ApiConfig, db: &DbConfig) -> anyhow::Result<sqlx::postgres::PgConnectOptions> {
    Ok(db
        .runtime(&db.control_db)?
        .options([("statement_timeout", format!("{}ms", cfg.statement_timeout_ms))]))
}

impl AppState {
    pub async fn new(cfg: ApiConfig, db: DbConfig) -> anyhow::Result<Self> {
        let db = Arc::new(db);
        // Uma conexão do orçamento do control fica com o PgListener.
        let control = crate::db::pool(
            "control",
            PgPoolOptions::new()
                .max_connections(cfg.control_pool_max - 1)
                .min_connections(0)
                .idle_timeout(Duration::from_secs(60))
                .test_before_acquire(false)
                .acquire_timeout(cfg.acquire_timeout),
            control_options(&cfg, &db)?,
            db.connect_retry,
        )
        .await?;
        let hasher = PasswordHasher::new(cfg.hash_concurrency, cfg.hash_wait)?;
        Ok(Self(Arc::new(Inner {
            registry: TenantRegistry::new(control.clone(), cfg.tenant_registry_capacity, cfg.cache_ttl),
            pools: TenantPools::new(
                db.clone(),
                cfg.max_open_tenant_pools,
                cfg.tenant_pool_max,
                cfg.acquire_timeout,
                cfg.statement_timeout_ms,
            ),
            access: AccessCache::new(cfg.access_cache_capacity, cfg.cache_ttl),
            platform: PlatformCache::new(cfg.platform_cache_capacity, cfg.cache_ttl),
            jwt: JwtKeys::new(&cfg),
            limiter: LoginLimiter::new(&cfg),
            hasher,
            control,
            db,
            cfg,
        })))
    }

    /// Falha se o pior caso de conexões ultrapassar `max_connections - reserva`.
    pub async fn check_connection_budget(&self) -> anyhow::Result<()> {
        let max: String = sqlx::query_scalar("SHOW max_connections")
            .fetch_one(&self.control)
            .await?;
        let max: u64 = max.parse()?;
        let budget = self.cfg.connection_budget();
        let available = max.saturating_sub(u64::from(self.cfg.pg_connection_reserve));
        if budget > available {
            bail!(
                "orçamento de conexões excedido: API_INSTANCES × (MAX_OPEN_TENANT_POOLS × \
                 TENANT_POOL_MAX_CONNECTIONS + CONTROL_POOL_MAX_CONNECTIONS) = {budget} > \
                 max_connections ({max}) - PG_CONNECTION_RESERVE ({})",
                self.cfg.pg_connection_reserve
            );
        }
        tracing::info!(budget, max_connections = max, "orçamento de conexões ok");
        Ok(())
    }

    pub async fn apply(&self, inv: &Invalidation) {
        match inv {
            Invalidation::Tenant { id, slug } => self.registry.invalidate(*id, slug).await,
            Invalidation::PlatformUser { id } => self.platform.invalidate(*id).await,
            Invalidation::TenantUser { tid, uid } => self.access.invalidate_user(*tid, *uid).await,
            Invalidation::TenantAll { tid } => self.access.invalidate_tenant(*tid),
        }
    }

    /// Depois do commit: invalida localmente e publica no `control` para as
    /// outras instâncias. Falha de publicação vira log + métrica; o TTL limita
    /// a defasagem.
    pub async fn publish(&self, inv: Invalidation) {
        self.apply(&inv).await;
        let payload = match serde_json::to_string(&inv) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!(error = %e, "falha ao serializar invalidação");
                return;
            }
        };
        let res = sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL_INVALIDATION)
            .bind(payload)
            .execute(&self.control)
            .await;
        match res {
            Ok(_) => metrics::counter!("cache_invalidations_published_total").increment(1),
            Err(e) => {
                metrics::counter!("cache_invalidation_publish_failures_total").increment(1);
                tracing::warn!(error = %e, "falha ao publicar invalidação; TTL limita a defasagem");
            }
        }
    }

    pub fn clear_caches(&self) {
        self.registry.clear();
        self.access.clear();
        self.platform.clear();
    }

    /// Um único `PgListener` por instância no `control`. Ao reconectar, limpa
    /// todos os caches (notificações podem ter sido perdidas).
    pub fn spawn_listener(&self) -> anyhow::Result<tokio::task::JoinHandle<()>> {
        let state = self.clone();
        let opts = self.db.runtime(&self.db.control_db)?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .connect_lazy_with(opts);
        Ok(tokio::spawn(async move {
            loop {
                let mut listener = match PgListener::connect_with(&pool).await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(error = %e, "listener: falha ao conectar");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        continue;
                    }
                };
                // Reconecta (e refaz o LISTEN) antes de devolver `None`, então
                // limpar os caches nesse ponto não perde notificações.
                listener.eager_reconnect(true);
                if let Err(e) = listener.listen(CHANNEL_INVALIDATION).await {
                    tracing::warn!(error = %e, "listener: falha no LISTEN");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
                state.clear_caches();
                metrics::counter!("cache_listener_connects_total").increment(1);
                loop {
                    match listener.try_recv().await {
                        Ok(Some(n)) => match serde_json::from_str::<Invalidation>(n.payload()) {
                            Ok(inv) => {
                                metrics::counter!("cache_invalidations_received_total").increment(1);
                                state.apply(&inv).await;
                            }
                            Err(e) => tracing::warn!(error = %e, "listener: payload inválido"),
                        },
                        Ok(None) => {
                            // Conexão perdida: o próximo try_recv reconecta e refaz o LISTEN.
                            tracing::warn!("listener: conexão perdida, limpando caches");
                            state.clear_caches();
                            metrics::counter!("cache_listener_connects_total").increment(1);
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "listener: erro, recriando");
                            state.clear_caches();
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            break;
                        }
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_payloads_parse() {
        let id = Uuid::now_v7();
        let t: Invalidation = serde_json::from_str(&format!(r#"{{"k":"tenant","id":"{id}","slug":"acme"}}"#)).unwrap();
        assert_eq!(
            t,
            Invalidation::Tenant {
                id,
                slug: "acme".into()
            }
        );
        let p: Invalidation = serde_json::from_str(&format!(r#"{{"k":"platform_user","id":"{id}"}}"#)).unwrap();
        assert_eq!(p, Invalidation::PlatformUser { id });
    }
}
