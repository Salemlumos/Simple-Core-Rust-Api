//! Infraestrutura dos testes de integração com PostgreSQL real.
//!
//! Cada teste cria o próprio banco `control` (`test_<uuid>`) e seus tenants,
//! então os testes rodam em paralelo sem interferência. As roles
//! `app_migrator`/`app_runtime` são compartilhadas (bootstrap idempotente).
//!
//! Configuração: variáveis `DB_*` do ambiente; se `DB_HOST` não estiver
//! definido, lê `.env.local`.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use simple_core::cli::seed::{SeedParams, SeedReport};
use simple_core::cli::{bootstrap, migrate, seed, worker};
use simple_core::config::{ApiConfig, DbConfig, RateLimit, Secret};
use simple_core::state::AppState;
use simple_core::tenancy::provision::no_failpoint;
use sqlx::{AssertSqlSafe, Connection, PgConnection};
use tower::ServiceExt;
use uuid::Uuid;

pub const OWNER_EMAIL: &str = "owner@test.local";
pub const OWNER_PASSWORD: &str = "owner-password-0123456789";
pub const SEED_SLUG: &str = "seed-tenant";
pub const PASSWORD: &str = "password-123456";

fn load_env_file() {
    if std::env::var("DB_HOST").is_ok() {
        return;
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/.env.local");
    let Ok(content) = std::fs::read_to_string(path) else {
        panic!("defina DB_* no ambiente ou crie .env.local (make db-up)");
    };
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && std::env::var(k).is_err()
        {
            // SAFETY: executado antes de qualquer thread de teste ler o ambiente.
            unsafe { std::env::set_var(k, v) };
        }
    }
}

static ENV_INIT: std::sync::Once = std::sync::Once::new();

pub struct Env {
    pub db: DbConfig,
    pub jwt_platform: String,
    pub jwt_tenant: String,
}

impl Env {
    /// Banco `control` novo, com roles garantidas e migrations aplicadas.
    pub async fn new() -> Env {
        ENV_INIT.call_once(load_env_file);
        let mut db = DbConfig::from_env().expect("configuração de banco");
        db.control_db = format!("test_{}", Uuid::now_v7().simple());
        db.connect_retry = Duration::from_secs(10);
        bootstrap::bootstrap(&db).await.expect("bootstrap");
        migrate::run_with(&db, 4).await.expect("migrate");
        Env {
            db,
            jwt_platform: format!("p-{}", Uuid::now_v7().simple()),
            jwt_tenant: format!("t-{}", Uuid::now_v7().simple()),
        }
    }

    /// `new` + seed (owner e tenant `seed-tenant`).
    pub async fn seeded() -> (Env, SeedReport) {
        let env = Env::new().await;
        let r = env.seed().await.expect("seed");
        (env, r)
    }

    pub fn seed_params(&self) -> SeedParams {
        SeedParams {
            owner_email: OWNER_EMAIL.into(),
            owner_name: "Owner".into(),
            owner_password: Secret::new(OWNER_PASSWORD),
            tenant_slug: SEED_SLUG.into(),
            tenant_name: "Seed".into(),
        }
    }

    pub async fn seed(&self) -> anyhow::Result<SeedReport> {
        seed::run_with(&self.db, &self.seed_params(), &no_failpoint).await
    }

    pub fn api_config(&self) -> ApiConfig {
        ApiConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            metrics_bind: "127.0.0.1:0".parse().unwrap(),
            api_docs: false,
            jwt_issuer: "simple-core-test".into(),
            jwt_platform_secret: Secret::new(self.jwt_platform.clone()),
            jwt_tenant_secret: Secret::new(self.jwt_tenant.clone()),
            access_ttl: Duration::from_secs(900),
            refresh_ttl: Duration::from_secs(30 * 24 * 3600),
            control_pool_max: 4,
            tenant_pool_max: 4,
            max_open_tenant_pools: 10,
            api_instances: 1,
            pg_connection_reserve: 0,
            access_cache_capacity: 10_000,
            platform_cache_capacity: 1_000,
            tenant_registry_capacity: 1_000,
            cache_ttl: Duration::from_secs(30),
            hash_concurrency: 4,
            hash_wait: Duration::from_secs(5),
            request_timeout: Duration::from_secs(10),
            acquire_timeout: Duration::from_secs(5),
            statement_timeout_ms: 5000,
            login_window: Duration::from_secs(60),
            login_limit_tenant: RateLimit {
                per_ip: 10_000,
                per_email: 1_000,
            },
            login_limit_platform: RateLimit {
                per_ip: 10_000,
                per_email: 1_000,
            },
        }
    }

    pub async fn app(&self) -> TestApp {
        self.app_with(self.api_config()).await
    }

    pub async fn app_with(&self, cfg: ApiConfig) -> TestApp {
        let state = AppState::new(cfg, self.db.clone()).await.expect("estado da api");
        let listener = state.spawn_listener().expect("listener");
        let router = simple_core::app::router(state.clone());
        TestApp {
            state,
            router,
            _listener: Arc::new(AbortOnDrop(listener)),
        }
    }

    pub async fn conn(&self, opts: sqlx::postgres::PgConnectOptions) -> PgConnection {
        PgConnection::connect_with(&opts).await.expect("conexão")
    }

    pub async fn admin(&self, db: &str) -> PgConnection {
        self.conn(self.db.admin(db).unwrap()).await
    }

    pub async fn runtime(&self, db: &str) -> PgConnection {
        self.conn(self.db.runtime(db).unwrap()).await
    }

    pub async fn migrator(&self, db: &str) -> PgConnection {
        self.conn(self.db.migrator(db).unwrap()).await
    }

    pub async fn control(&self) -> PgConnection {
        self.migrator(&self.db.control_db).await
    }

    pub async fn db_name(&self, tenant: Uuid) -> String {
        let mut c = self.control().await;
        sqlx::query_scalar("SELECT db_name FROM tenants WHERE id = $1")
            .bind(tenant)
            .fetch_one(&mut c)
            .await
            .unwrap()
    }

    pub async fn tenant_status(&self, tenant: Uuid) -> String {
        let mut c = self.control().await;
        sqlx::query_scalar("SELECT status FROM tenants WHERE id = $1")
            .bind(tenant)
            .fetch_one(&mut c)
            .await
            .unwrap()
    }

    pub async fn drain_worker(&self) {
        worker::drain(&self.db, &no_failpoint).await.expect("worker");
    }

    /// Remove o control de teste e os bancos dos seus tenants.
    pub async fn cleanup(self) {
        let mut admin = self.admin(&self.db.admin_db).await;
        let mut ctl = self.control().await;
        let dbs: Vec<String> = sqlx::query_scalar("SELECT db_name FROM tenants")
            .fetch_all(&mut ctl)
            .await
            .unwrap_or_default();
        drop(ctl);
        for d in dbs.iter().chain(std::iter::once(&self.db.control_db)) {
            let _ = sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE IF EXISTS \"{d}\" WITH (FORCE)")))
                .execute(&mut admin)
                .await;
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Clone)]
pub struct TestApp {
    pub state: AppState,
    router: Router,
    _listener: Arc<AbortOnDrop>,
}

#[derive(Debug)]
pub struct Resp {
    pub status: StatusCode,
    pub body: Value,
    pub content_type: Option<String>,
}

impl Resp {
    pub fn id(&self) -> Uuid {
        self.body["id"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("sem id: {self:?}"))
    }
}

#[derive(Default, Clone, Copy)]
pub struct Auth<'a> {
    pub token: Option<&'a str>,
    pub slug: Option<&'a str>,
}

pub fn tok(token: &str) -> Auth<'_> {
    Auth {
        token: Some(token),
        slug: None,
    }
}

pub fn tok_slug<'a>(token: &'a str, slug: &'a str) -> Auth<'a> {
    Auth {
        token: Some(token),
        slug: Some(slug),
    }
}

pub const NONE: Auth<'static> = Auth {
    token: None,
    slug: None,
};

impl TestApp {
    pub async fn call(&self, method: Method, path: &str, auth: Auth<'_>, body: Option<Value>) -> Resp {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(t) = auth.token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(s) = auth.slug {
            req = req.header("x-tenant-slug", s);
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let resp = self.router.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Resp {
            status,
            body,
            content_type,
        }
    }

    pub async fn get(&self, path: &str, auth: Auth<'_>) -> Resp {
        self.call(Method::GET, path, auth, None).await
    }
    pub async fn post(&self, path: &str, auth: Auth<'_>, body: Value) -> Resp {
        self.call(Method::POST, path, auth, Some(body)).await
    }
    pub async fn patch(&self, path: &str, auth: Auth<'_>, body: Value) -> Resp {
        self.call(Method::PATCH, path, auth, Some(body)).await
    }
    pub async fn put(&self, path: &str, auth: Auth<'_>, body: Value) -> Resp {
        self.call(Method::PUT, path, auth, Some(body)).await
    }
    pub async fn delete(&self, path: &str, auth: Auth<'_>) -> Resp {
        self.call(Method::DELETE, path, auth, None).await
    }

    // ------------------------------------------------------------ fluxos

    pub async fn platform_login(&self, email: &str, password: &str) -> (String, String) {
        let r = self
            .post(
                "/v1/platform/auth/login",
                NONE,
                json!({ "email": email, "password": password }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "login de plataforma: {r:?}");
        (
            r.body["access_token"].as_str().unwrap().into(),
            r.body["refresh_token"].as_str().unwrap().into(),
        )
    }

    pub async fn owner(&self) -> String {
        self.platform_login(OWNER_EMAIL, OWNER_PASSWORD).await.0
    }

    pub async fn create_superuser(&self, owner: &str, email: &str) -> (Uuid, String) {
        let r = self
            .post(
                "/v1/platform/superusers",
                tok(owner),
                json!({ "email": email, "name": "Super", "password": PASSWORD }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{r:?}");
        let token = self.platform_login(email, PASSWORD).await.0;
        (r.id(), token)
    }

    /// Cria e provisiona (worker) um tenant ativo.
    pub async fn create_tenant(&self, env: &Env, owner: &str, slug: &str) -> Uuid {
        let r = self
            .post(
                "/v1/platform/tenants",
                tok(owner),
                json!({ "slug": slug, "name": slug }),
            )
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{r:?}");
        env.drain_worker().await;
        let id = r.id();
        assert_eq!(env.tenant_status(id).await, "active");
        id
    }

    pub async fn create_admin(&self, owner: &str, tenant: Uuid, email: &str) -> Uuid {
        let r = self
            .post(
                &format!("/v1/platform/tenants/{tenant}/admins"),
                tok(owner),
                json!({ "email": email, "name": "Admin", "password": PASSWORD }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{r:?}");
        r.id()
    }

    pub async fn tenant_login_raw(&self, slug: &str, email: &str, password: &str) -> Resp {
        self.call(
            Method::POST,
            "/v1/auth/login",
            Auth {
                token: None,
                slug: Some(slug),
            },
            Some(json!({ "email": email, "password": password })),
        )
        .await
    }

    pub async fn tenant_login(&self, slug: &str, email: &str) -> (String, String) {
        let r = self.tenant_login_raw(slug, email, PASSWORD).await;
        assert_eq!(r.status, StatusCode::OK, "login de tenant: {r:?}");
        (
            r.body["access_token"].as_str().unwrap().into(),
            r.body["refresh_token"].as_str().unwrap().into(),
        )
    }

    pub async fn create_user(&self, auth: Auth<'_>, email: &str) -> Uuid {
        let r = self
            .post(
                "/v1/users",
                auth,
                json!({ "email": email, "name": "User", "password": PASSWORD }),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{r:?}");
        r.id()
    }

    pub async fn create_profile(&self, auth: Auth<'_>, name: &str, features: &[&str]) -> Uuid {
        let r = self
            .post("/v1/profiles", auth, json!({ "name": name, "features": features }))
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{r:?}");
        r.id()
    }

    pub async fn set_profiles(&self, auth: Auth<'_>, user: Uuid, profiles: &[Uuid]) -> Resp {
        self.put(
            &format!("/v1/users/{user}/profiles"),
            auth,
            json!({ "profile_ids": profiles }),
        )
        .await
    }

    /// Usuário comum com as features dadas (via perfil dedicado) e seu token.
    pub async fn user_with(&self, admin: Auth<'_>, slug: &str, email: &str, features: &[&str]) -> (Uuid, String) {
        let id = self.create_user(admin, email).await;
        if !features.is_empty() {
            let p = self.create_profile(admin, &format!("p-{email}"), features).await;
            let r = self.set_profiles(admin, id, &[p]).await;
            assert_eq!(r.status, StatusCode::OK, "{r:?}");
        }
        (id, self.tenant_login(slug, email).await.0)
    }
}

/// Repete `f` até retornar `true` (máx. 5 s).
pub async fn eventually<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if f().await {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "timeout esperando: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Ambiente semeado + app + tenant `seed-tenant` com um admin logado.
pub struct World {
    pub env: Env,
    pub app: TestApp,
    pub owner: String,
    pub tenant: Uuid,
    pub admin_id: Uuid,
    pub admin: String,
}

impl World {
    pub async fn new() -> World {
        let (env, report) = Env::seeded().await;
        let app = env.app().await;
        let owner = app.owner().await;
        let admin_id = app.create_admin(&owner, report.tenant_id, "admin@seed.io").await;
        let admin = app.tenant_login(SEED_SLUG, "admin@seed.io").await.0;
        World {
            env,
            app,
            owner,
            tenant: report.tenant_id,
            admin_id,
            admin,
        }
    }

    pub fn admin(&self) -> Auth<'_> {
        tok(&self.admin)
    }

    pub async fn cleanup(self) {
        drop(self.app);
        self.env.cleanup().await;
    }
}
