//! Configuração exclusivamente por variáveis de ambiente.
//!
//! Cada comando lê apenas o que precisa: a API nunca exige credenciais do
//! `app_migrator`, e só o `bootstrap` lê as credenciais administrativas.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, bail};
use sqlx::ConnectOptions;
use sqlx::postgres::{PgConnectOptions, PgSslMode};

/// Segredo que nunca aparece em `Debug`/logs.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

pub fn env_opt(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

pub fn env_req(name: &str) -> anyhow::Result<String> {
    env_opt(name).with_context(|| format!("variável de ambiente obrigatória ausente: {name}"))
}

pub fn env_parse<T: FromStr>(name: &str, default: T) -> anyhow::Result<T>
where
    T::Err: fmt::Display,
{
    match env_opt(name) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|e| anyhow::anyhow!("valor inválido em {name}: {e}")),
    }
}

pub fn env_bool(name: &str, default: bool) -> anyhow::Result<bool> {
    match env_opt(name).as_deref() {
        None => Ok(default),
        Some("1" | "true" | "TRUE" | "yes") => Ok(true),
        Some("0" | "false" | "FALSE" | "no") => Ok(false),
        Some(v) => bail!("valor inválido em {name}: {v}"),
    }
}

#[derive(Clone, Debug)]
pub struct Credentials {
    pub user: String,
    pub password: Secret,
}

/// Parâmetros de conexão. Não existe `DATABASE_URL`: as conexões são montadas
/// por banco (control ou tenant_*) e por role.
#[derive(Clone, Debug)]
pub struct DbConfig {
    pub host: String,
    pub port: u16,
    pub control_db: String,
    pub admin_db: String,
    pub ssl_mode: PgSslMode,
    pub ssl_root_cert: Option<PathBuf>,
    pub connect_retry: Duration,
    /// Nome da role de runtime (usado também pelo migrator para conceder privilégios).
    pub runtime_user: String,
    pub runtime_password: Option<Secret>,
    pub migrator_user: String,
    pub migrator_password: Option<Secret>,
    pub admin_user: Option<String>,
    pub admin_password: Option<Secret>,
}

impl DbConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let ssl_mode = match env_opt("DB_SSLMODE").as_deref().unwrap_or("prefer") {
            "disable" => PgSslMode::Disable,
            "allow" => PgSslMode::Allow,
            "prefer" => PgSslMode::Prefer,
            "require" => PgSslMode::Require,
            "verify-ca" => PgSslMode::VerifyCa,
            "verify-full" => PgSslMode::VerifyFull,
            other => bail!("DB_SSLMODE inválido: {other}"),
        };
        let cfg = Self {
            host: env_req("DB_HOST")?,
            port: env_parse("DB_PORT", 5432u16)?,
            control_db: env_opt("DB_CONTROL_NAME").unwrap_or_else(|| "control".into()),
            admin_db: env_opt("DB_ADMIN_DATABASE").unwrap_or_else(|| "postgres".into()),
            ssl_mode,
            ssl_root_cert: env_opt("DB_SSL_ROOT_CERT").map(PathBuf::from),
            connect_retry: Duration::from_secs(env_parse("DB_CONNECT_RETRY_SECONDS", 30u64)?),
            runtime_user: env_opt("DB_RUNTIME_USER").unwrap_or_else(|| "app_runtime".into()),
            runtime_password: env_opt("DB_RUNTIME_PASSWORD").map(Secret),
            migrator_user: env_opt("DB_MIGRATOR_USER").unwrap_or_else(|| "app_migrator".into()),
            migrator_password: env_opt("DB_MIGRATOR_PASSWORD").map(Secret),
            admin_user: env_opt("DB_ADMIN_USER"),
            admin_password: env_opt("DB_ADMIN_PASSWORD").map(Secret),
        };
        crate::db::validate_role_name(&cfg.runtime_user)?;
        crate::db::validate_role_name(&cfg.migrator_user)?;
        crate::db::validate_db_name(&cfg.control_db)?;
        Ok(cfg)
    }

    fn base(&self, db: &str) -> PgConnectOptions {
        let mut o = PgConnectOptions::new_without_pgpass()
            .host(&self.host)
            .port(self.port)
            .database(db)
            .ssl_mode(self.ssl_mode)
            .application_name("simple-core")
            // Statements nunca vão para o log no nível INFO; lentos viram WARN.
            .log_statements(log::LevelFilter::Trace)
            .log_slow_statements(log::LevelFilter::Warn, Duration::from_secs(1));
        if let Some(ca) = &self.ssl_root_cert {
            o = o.ssl_root_cert(ca);
        }
        o
    }

    fn with(&self, db: &str, user: &str, pass: Option<&Secret>, what: &str) -> anyhow::Result<PgConnectOptions> {
        let pass = pass.with_context(|| format!("senha da role {what} não configurada"))?;
        Ok(self.base(db).username(user).password(pass.expose()))
    }

    pub fn runtime(&self, db: &str) -> anyhow::Result<PgConnectOptions> {
        self.with(
            db,
            &self.runtime_user,
            self.runtime_password.as_ref(),
            "DB_RUNTIME_PASSWORD",
        )
    }

    pub fn migrator(&self, db: &str) -> anyhow::Result<PgConnectOptions> {
        self.with(
            db,
            &self.migrator_user,
            self.migrator_password.as_ref(),
            "DB_MIGRATOR_PASSWORD",
        )
    }

    pub fn admin(&self, db: &str) -> anyhow::Result<PgConnectOptions> {
        let user = self.admin_user.as_deref().context("DB_ADMIN_USER não configurado")?;
        Ok(self
            .with(db, user, self.admin_password.as_ref(), "DB_ADMIN_PASSWORD")?
            // O bootstrap envia verificadores SCRAM; ainda assim nada de log de SQL.
            .log_statements(log::LevelFilter::Off)
            .log_slow_statements(log::LevelFilter::Off, Duration::from_secs(60)))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RateLimit {
    pub per_ip: u32,
    pub per_email: u32,
}

/// Configuração do processo `api serve`.
#[derive(Clone, Debug)]
pub struct ApiConfig {
    pub bind: SocketAddr,
    pub metrics_bind: SocketAddr,
    pub jwt_issuer: String,
    pub jwt_platform_secret: Secret,
    pub jwt_tenant_secret: Secret,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
    pub control_pool_max: u32,
    pub tenant_pool_max: u32,
    pub max_open_tenant_pools: u64,
    pub api_instances: u32,
    pub pg_connection_reserve: u32,
    pub access_cache_capacity: u64,
    pub platform_cache_capacity: u64,
    pub tenant_registry_capacity: u64,
    pub cache_ttl: Duration,
    pub hash_concurrency: usize,
    pub hash_wait: Duration,
    pub request_timeout: Duration,
    pub acquire_timeout: Duration,
    pub statement_timeout_ms: u64,
    pub login_window: Duration,
    pub login_limit_tenant: RateLimit,
    pub login_limit_platform: RateLimit,
}

pub const PLATFORM_AUDIENCE: &str = "simple-core:platform";
pub const TENANT_AUDIENCE: &str = "simple-core:tenant";

impl ApiConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        let cfg = Self {
            bind: env_parse("API_BIND", "127.0.0.1:8080".parse::<SocketAddr>()?)?,
            metrics_bind: env_parse("METRICS_BIND", "127.0.0.1:9090".parse::<SocketAddr>()?)?,
            jwt_issuer: env_opt("JWT_ISSUER").unwrap_or_else(|| "simple-core".into()),
            jwt_platform_secret: Secret(env_req("JWT_PLATFORM_SECRET")?),
            jwt_tenant_secret: Secret(env_req("JWT_TENANT_SECRET")?),
            access_ttl: Duration::from_secs(15 * 60),
            refresh_ttl: Duration::from_secs(30 * 24 * 3600),
            control_pool_max: env_parse("CONTROL_POOL_MAX_CONNECTIONS", 10u32)?,
            tenant_pool_max: env_parse("TENANT_POOL_MAX_CONNECTIONS", 10u32)?,
            max_open_tenant_pools: env_parse("MAX_OPEN_TENANT_POOLS", 50u64)?,
            api_instances: env_parse("API_INSTANCES", 1u32)?,
            pg_connection_reserve: env_parse("PG_CONNECTION_RESERVE", 10u32)?,
            access_cache_capacity: env_parse("ACCESS_CACHE_CAPACITY", 200_000u64)?,
            platform_cache_capacity: env_parse("PLATFORM_CACHE_CAPACITY", 10_000u64)?,
            tenant_registry_capacity: env_parse("TENANT_REGISTRY_CAPACITY", 10_000u64)?,
            cache_ttl: Duration::from_secs(30),
            hash_concurrency: env_parse("PASSWORD_HASH_CONCURRENCY", cores)?,
            hash_wait: Duration::from_secs(1),
            request_timeout: Duration::from_secs(3),
            acquire_timeout: Duration::from_secs(1),
            statement_timeout_ms: 2000,
            login_window: Duration::from_secs(env_parse("LOGIN_RATE_WINDOW_SECONDS", 60u64)?),
            login_limit_tenant: RateLimit {
                per_ip: env_parse("LOGIN_RATE_TENANT_PER_IP", 60u32)?,
                per_email: env_parse("LOGIN_RATE_TENANT_PER_EMAIL", 10u32)?,
            },
            login_limit_platform: RateLimit {
                per_ip: env_parse("LOGIN_RATE_PLATFORM_PER_IP", 20u32)?,
                per_email: env_parse("LOGIN_RATE_PLATFORM_PER_EMAIL", 5u32)?,
            },
        };
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.jwt_platform_secret.expose().len() < 32 || self.jwt_tenant_secret.expose().len() < 32 {
            bail!("JWT_PLATFORM_SECRET e JWT_TENANT_SECRET precisam ter pelo menos 32 bytes");
        }
        if self.jwt_platform_secret.expose() == self.jwt_tenant_secret.expose() {
            bail!("JWT_PLATFORM_SECRET e JWT_TENANT_SECRET precisam ser diferentes");
        }
        if self.control_pool_max < 2 {
            bail!("CONTROL_POOL_MAX_CONNECTIONS precisa ser >= 2 (uma conexão é do listener)");
        }
        if self.tenant_pool_max < 1 || self.max_open_tenant_pools < 1 || self.hash_concurrency < 1 {
            bail!("limites de pool/hash precisam ser >= 1");
        }
        let lp = self.login_limit_platform;
        let lt = self.login_limit_tenant;
        if lp.per_ip > lt.per_ip || lp.per_email > lt.per_email {
            bail!("o limite de login da plataforma precisa ser mais restritivo que o de tenant");
        }
        Ok(())
    }

    /// Conexões que cada instância pode abrir no pior caso.
    pub fn connection_budget(&self) -> u64 {
        u64::from(self.api_instances)
            * (self.max_open_tenant_pools * u64::from(self.tenant_pool_max) + u64::from(self.control_pool_max))
    }
}
