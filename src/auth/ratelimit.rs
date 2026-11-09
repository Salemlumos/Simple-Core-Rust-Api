//! Limite de tentativas de login por escopo (plataforma ou tenant), IP e e-mail.
//! Janela fixa: a entrada expira `LOGIN_RATE_WINDOW_SECONDS` após ser criada.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use moka::future::Cache;

use crate::config::{ApiConfig, RateLimit};
use crate::error::{AppError, AppResult};

#[derive(Clone, Copy)]
pub enum Scope<'a> {
    Platform,
    Tenant(&'a str),
}

#[derive(Clone)]
pub struct LoginLimiter {
    counters: Cache<String, Arc<AtomicU32>>,
    tenant: RateLimit,
    platform: RateLimit,
}

impl LoginLimiter {
    pub fn new(cfg: &ApiConfig) -> Self {
        Self {
            counters: Cache::builder()
                .max_capacity(1_000_000)
                .time_to_live(cfg.login_window)
                .build(),
            tenant: cfg.login_limit_tenant,
            platform: cfg.login_limit_platform,
        }
    }

    async fn hit(&self, key: String, limit: u32) -> bool {
        let c = self.counters.get_with(key, async { Arc::new(AtomicU32::new(0)) }).await;
        c.fetch_add(1, Ordering::Relaxed) < limit
    }

    pub async fn check(&self, scope: Scope<'_>, ip: IpAddr, email: &str) -> AppResult<()> {
        let (prefix, limit) = match scope {
            Scope::Platform => ("p".to_string(), self.platform),
            Scope::Tenant(slug) => (format!("t:{slug}"), self.tenant),
        };
        let ip_ok = self.hit(format!("{prefix}|ip|{ip}"), limit.per_ip).await;
        let email_ok = self.hit(format!("{prefix}|email|{email}"), limit.per_email).await;
        if ip_ok && email_ok {
            Ok(())
        } else {
            metrics::counter!(
                "login_rate_limited_total",
                "scope" => if matches!(scope, Scope::Platform) { "platform" } else { "tenant" }
            )
            .increment(1);
            Err(AppError::TooManyRequests)
        }
    }
}
