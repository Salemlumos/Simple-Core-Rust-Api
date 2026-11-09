//! Caches de autorização em `moka`: TTL de 30 s, capacidade configurável e
//! coalescência de cargas simultâneas com `try_get_with`.

use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use sqlx::PgPool;
use uuid::Uuid;

use super::resolver::{self, PlatformPrincipal, Principal};

pub fn record(cache: &'static str, hit: bool) {
    let result = if hit { "hit" } else { "miss" };
    metrics::counter!("cache_requests_total", "cache" => cache, "result" => result).increment(1);
}

/// `(tenant_id, user_id)` → `Arc<Principal>` imutável (ou `None` se o usuário não existe).
#[derive(Clone)]
pub struct AccessCache {
    cache: Cache<(Uuid, Uuid), Option<Arc<Principal>>>,
}

impl AccessCache {
    pub fn new(capacity: u64, ttl: Duration) -> Self {
        Self {
            cache: Cache::builder()
                .max_capacity(capacity)
                .time_to_live(ttl)
                .support_invalidation_closures()
                .build(),
        }
    }

    pub async fn get(
        &self,
        tenant: Uuid,
        user: Uuid,
        pool: &PgPool,
    ) -> Result<Option<Arc<Principal>>, Arc<sqlx::Error>> {
        let mut loaded = false;
        let v = self
            .cache
            .try_get_with((tenant, user), async {
                loaded = true;
                resolver::load_principal(pool, user).await.map(|p| p.map(Arc::new))
            })
            .await;
        record("access", !loaded);
        v
    }

    pub async fn invalidate_user(&self, tenant: Uuid, user: Uuid) {
        self.cache.invalidate(&(tenant, user)).await;
    }

    /// Usado em testes para verificar o escopo das invalidações.
    pub fn contains(&self, tenant: Uuid, user: Uuid) -> bool {
        self.cache.contains_key(&(tenant, user))
    }

    pub fn invalidate_tenant(&self, tenant: Uuid) {
        // Só falha se o builder não suportar closures, o que é configurado acima.
        let _ = self.cache.invalidate_entries_if(move |k, _| k.0 == tenant);
    }

    pub fn clear(&self) {
        self.cache.invalidate_all();
    }
}

/// `user_id` → papel e status do usuário de plataforma.
#[derive(Clone)]
pub struct PlatformCache {
    cache: Cache<Uuid, Option<Arc<PlatformPrincipal>>>,
}

impl PlatformCache {
    pub fn new(capacity: u64, ttl: Duration) -> Self {
        Self {
            cache: Cache::builder().max_capacity(capacity).time_to_live(ttl).build(),
        }
    }

    pub async fn get(&self, user: Uuid, pool: &PgPool) -> Result<Option<Arc<PlatformPrincipal>>, Arc<sqlx::Error>> {
        let mut loaded = false;
        let v = self
            .cache
            .try_get_with(user, async {
                loaded = true;
                resolver::load_platform_principal(pool, user)
                    .await
                    .map(|p| p.map(Arc::new))
            })
            .await;
        record("platform", !loaded);
        v
    }

    pub async fn invalidate(&self, user: Uuid) {
        self.cache.invalidate(&user).await;
    }

    pub fn clear(&self) {
        self.cache.invalidate_all();
    }
}
