//! Limite de tentativas de login por escopo (plataforma ou tenant), IP e e-mail.
//! Janela fixa: a entrada expira `LOGIN_RATE_WINDOW_SECONDS` após ser criada.

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use moka::future::Cache;

use crate::config::{ApiConfig, RateLimit};
use crate::error::{AppError, AppResult};

#[derive(Clone, Copy)]
pub enum Scope<'a> {
    Platform,
    Tenant(&'a str),
}

/// Contador de uma janela: guarda quando nasceu para calcular o tempo que falta.
struct Entry {
    created: Instant,
    count: AtomicU32,
}

#[derive(Clone)]
pub struct LoginLimiter {
    counters: Cache<String, Arc<Entry>>,
    window: Duration,
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
            window: cfg.login_window,
            tenant: cfg.login_limit_tenant,
            platform: cfg.login_limit_platform,
        }
    }

    /// Conta a tentativa; ao estourar, devolve o tempo que falta para a janela da entrada acabar.
    async fn hit(&self, key: String, limit: u32) -> Result<(), Duration> {
        let e = self
            .counters
            .get_with(key, async {
                Arc::new(Entry {
                    created: Instant::now(),
                    count: AtomicU32::new(0),
                })
            })
            .await;
        if e.count.fetch_add(1, Ordering::Relaxed) < limit {
            Ok(())
        } else {
            Err(self.window.saturating_sub(e.created.elapsed()))
        }
    }

    pub async fn check(&self, scope: Scope<'_>, ip: IpAddr, email: &str) -> AppResult<()> {
        let (prefix, limit) = match scope {
            Scope::Platform => ("p".to_string(), self.platform),
            Scope::Tenant(slug) => (format!("t:{slug}"), self.tenant),
        };
        let ip_res = self.hit(format!("{prefix}|ip|{ip}"), limit.per_ip).await;
        let email_res = self.hit(format!("{prefix}|email|{email}"), limit.per_email).await;
        let wait = match (ip_res, email_res) {
            (Ok(()), Ok(())) => return Ok(()),
            (Err(a), Err(b)) => a.max(b),
            (Err(a), Ok(())) | (Ok(()), Err(a)) => a,
        };
        metrics::counter!(
            "login_rate_limited_total",
            "scope" => if matches!(scope, Scope::Platform) { "platform" } else { "tenant" }
        )
        .increment(1);
        let retry_after_secs = wait.as_secs_f64().ceil() as u64;
        Err(AppError::TooManyRequests {
            retry_after_secs: retry_after_secs.clamp(1, self.window.as_secs().max(1)),
        })
    }
}
