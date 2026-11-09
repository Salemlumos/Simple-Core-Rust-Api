//! Registro de tenants em cache (TTL 30 s), invalidado por LISTEN/NOTIFY no `control`.

use std::sync::Arc;
use std::time::Duration;

use moka::future::Cache;
use sqlx::PgPool;
use uuid::Uuid;

use crate::access::cache::record;
use crate::db::is_valid_slug;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TenantStatus {
    Provisioning,
    Active,
    Suspended,
    Failed,
}

impl TenantStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "active" => Self::Active,
            "suspended" => Self::Suspended,
            "provisioning" => Self::Provisioning,
            _ => Self::Failed,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TenantInfo {
    pub id: Uuid,
    pub slug: String,
    pub db_name: String,
    pub status: TenantStatus,
}

type Entry = Option<Arc<TenantInfo>>;

#[derive(Clone)]
pub struct TenantRegistry {
    control: PgPool,
    by_slug: Cache<String, Entry>,
    by_id: Cache<Uuid, Entry>,
}

impl TenantRegistry {
    pub fn new(control: PgPool, capacity: u64, ttl: Duration) -> Self {
        Self {
            control,
            by_slug: Cache::builder().max_capacity(capacity).time_to_live(ttl).build(),
            by_id: Cache::builder().max_capacity(capacity).time_to_live(ttl).build(),
        }
    }

    pub async fn by_slug(&self, slug: &str) -> Result<Entry, Arc<sqlx::Error>> {
        // Slug inválido nunca chega ao banco nem ocupa o cache.
        if !is_valid_slug(slug) {
            return Ok(None);
        }
        let mut loaded = false;
        let v = self
            .by_slug
            .try_get_with_by_ref(slug, async {
                loaded = true;
                sqlx::query!("SELECT id, slug, db_name, status FROM tenants WHERE slug = $1", slug)
                    .fetch_optional(&self.control)
                    .await
                    .map(|r| {
                        r.map(|r| {
                            Arc::new(TenantInfo {
                                id: r.id,
                                slug: r.slug,
                                db_name: r.db_name,
                                status: TenantStatus::parse(&r.status),
                            })
                        })
                    })
            })
            .await;
        record("tenant_registry", !loaded);
        v
    }

    pub async fn by_id(&self, id: Uuid) -> Result<Entry, Arc<sqlx::Error>> {
        let mut loaded = false;
        let v = self
            .by_id
            .try_get_with(id, async {
                loaded = true;
                sqlx::query!("SELECT id, slug, db_name, status FROM tenants WHERE id = $1", id)
                    .fetch_optional(&self.control)
                    .await
                    .map(|r| {
                        r.map(|r| {
                            Arc::new(TenantInfo {
                                id: r.id,
                                slug: r.slug,
                                db_name: r.db_name,
                                status: TenantStatus::parse(&r.status),
                            })
                        })
                    })
            })
            .await;
        record("tenant_registry", !loaded);
        v
    }

    pub async fn invalidate(&self, id: Uuid, slug: &str) {
        self.by_id.invalidate(&id).await;
        self.by_slug.invalidate(slug).await;
    }

    pub fn clear(&self) {
        self.by_id.invalidate_all();
        self.by_slug.invalidate_all();
    }
}
