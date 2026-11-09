//! Operação: seed, worker, provisionamento retomável, migrate e pools.

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use simple_core::cli::{migrate, worker};
use simple_core::tenancy::provision::{Outcome, Step, no_failpoint};
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn seed_twice_and_concurrently_creates_one_owner_and_one_base() {
    let env = Env::new().await;
    // Quatro seeds simultâneos (futures concorrentes, conexões independentes).
    let p = env.seed_params();
    let reports: Vec<_> =
        futures::future::join_all((0..4).map(|_| simple_core::cli::seed::run_with(&env.db, &p, &no_failpoint)))
            .await
            .into_iter()
            .map(Result::unwrap)
            .collect();
    assert_eq!(reports.iter().filter(|r| r.owner_created).count(), 1);
    assert_eq!(reports.iter().filter(|r| r.tenant_created).count(), 1);
    assert_eq!(
        reports.iter().filter(|r| r.provisioning == Outcome::Activated).count(),
        1
    );
    let tid = reports[0].tenant_id;
    assert!(reports.iter().all(|r| r.tenant_id == tid));

    let mut ctl = env.control().await;
    let hash: String = sqlx::query_scalar("SELECT password_hash FROM platform_users WHERE role = 'owner'")
        .fetch_one(&mut ctl)
        .await
        .unwrap();
    let db_oid: u32 = {
        let mut admin = env.admin(&env.db.admin_db).await;
        let name = env.db_name(tid).await;
        sqlx::query_scalar::<_, sqlx::postgres::types::Oid>("SELECT oid FROM pg_database WHERE datname = $1")
            .bind(name)
            .fetch_one(&mut admin)
            .await
            .unwrap()
            .0
    };

    // Reexecução com outra senha: nada muda.
    let mut p = env.seed_params();
    p.owner_password = simple_core::config::Secret::new("outra-senha-bem-longa");
    let again = simple_core::cli::seed::run_with(&env.db, &p, &no_failpoint)
        .await
        .unwrap();
    assert!(!again.owner_created && !again.tenant_created);
    assert_eq!(again.provisioning, Outcome::AlreadyActive);
    let (owners, tenants, hash2): (i64, i64, String) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM platform_users WHERE role = 'owner'),
                (SELECT count(*) FROM tenants),
                (SELECT password_hash FROM platform_users WHERE role = 'owner')",
    )
    .fetch_one(&mut ctl)
    .await
    .unwrap();
    assert_eq!((owners, tenants), (1, 1));
    assert_eq!(hash, hash2, "senha do owner não pode mudar");
    let mut admin = env.admin(&env.db.admin_db).await;
    let oid2: sqlx::postgres::types::Oid = sqlx::query_scalar("SELECT oid FROM pg_database WHERE datname = $1")
        .bind(env.db_name(tid).await)
        .fetch_one(&mut admin)
        .await
        .unwrap();
    assert_eq!(db_oid, oid2.0, "banco não pode ser recriado");

    // Seed com e-mail de superusuário existente falha sem promovê-lo.
    let env2 = Env::new().await;
    let mut c2 = env2.control().await;
    sqlx::query(
        "INSERT INTO platform_users (id, email, name, password_hash, role) VALUES ($1, $2, 'su', 'x', 'superuser')",
    )
    .bind(Uuid::now_v7())
    .bind(OWNER_EMAIL)
    .execute(&mut c2)
    .await
    .unwrap();
    assert!(env2.seed().await.is_err());
    let role: String = sqlx::query_scalar("SELECT role FROM platform_users WHERE email = $1")
        .bind(OWNER_EMAIL)
        .fetch_one(&mut c2)
        .await
        .unwrap();
    assert_eq!(role, "superuser");
    env2.cleanup().await;
    env.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn two_workers_compete_for_the_same_tenant() {
    let w = World::new().await;
    let r = w
        .app
        .post(
            "/v1/platform/tenants",
            tok(&w.owner),
            json!({ "slug": "contended", "name": "c" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    let tid = r.id();
    let (a, b) = tokio::join!(
        worker::process_one(&w.env.db, &no_failpoint),
        worker::process_one(&w.env.db, &no_failpoint)
    );
    let results = [a.unwrap(), b.unwrap()];
    let activated = results
        .iter()
        .flatten()
        .filter(|(id, o)| *id == tid && *o == Outcome::Activated)
        .count();
    assert_eq!(activated, 1, "{results:?}");
    assert_eq!(w.env.tenant_status(tid).await, "active");
    // Fila vazia depois.
    assert!(worker::process_one(&w.env.db, &no_failpoint).await.unwrap().is_none());
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn provisioning_interrupted_at_each_step_resumes() {
    let w = World::new().await;
    let app = &w.app;
    for (i, step) in Step::ALL.iter().enumerate() {
        let slug = format!("interrupted-{i}");
        let r = app
            .post(
                "/v1/platform/tenants",
                tok(&w.owner),
                json!({ "slug": slug, "name": "x" }),
            )
            .await;
        let tid = r.id();
        let fail_at = *step;
        let fp = move |s: Step| {
            if s == fail_at {
                anyhow::bail!("interrompido em {s:?}")
            } else {
                Ok(())
            }
        };
        let (id, out) = worker::process_one(&w.env.db, &fp).await.unwrap().unwrap();
        assert_eq!(id, tid);
        assert!(
            matches!(out, Outcome::Failed(ref m) if m.contains("interrompido")),
            "{out:?}"
        );
        let t = app.get(&format!("/v1/platform/tenants/{tid}"), tok(&w.owner)).await;
        assert_eq!(t.body["status"], "failed");
        assert!(t.body["last_error"].as_str().unwrap().contains("interrompido"));
        // Tenant com falha não atende requisições.
        assert_eq!(
            app.tenant_login_raw(&slug, "a@x.io", PASSWORD).await.status,
            StatusCode::UNAUTHORIZED
        );

        // Retomada.
        let r = app
            .post(&format!("/v1/platform/tenants/{tid}/retry"), tok(&w.owner), json!({}))
            .await;
        assert_eq!(r.status, StatusCode::ACCEPTED);
        let r = app
            .post(&format!("/v1/platform/tenants/{tid}/retry"), tok(&w.owner), json!({}))
            .await;
        assert_eq!(r.status, StatusCode::CONFLICT, "só tenants failed");
        w.env.drain_worker().await;
        assert_eq!(w.env.tenant_status(tid).await, "active", "{step:?}");
        let admin = app.create_admin(&w.owner, tid, "a@x.io").await;
        let t = app.tenant_login(&slug, "a@x.io").await.0;
        assert_eq!(
            app.get(&format!("/v1/users/{admin}"), tok(&t)).await.status,
            StatusCode::OK
        );
        assert_eq!(app.get("/v1/modules", tok(&t)).await.body.as_array().unwrap().len(), 2);
    }
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migrate_continues_when_one_tenant_fails_and_reports_error() {
    let w = World::new().await;
    let app = &w.app;
    let good = app.create_tenant(&w.env, &w.owner, "good").await;
    let bad = app.create_tenant(&w.env, &w.owner, "bad").await;
    let runtime = w.env.db.runtime_user.clone();

    // Tenants bons perdem um privilégio (o migrate deve reaplicar).
    for t in [w.tenant, good] {
        let mut c = w.env.migrator(&w.env.db_name(t).await).await;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!("REVOKE SELECT ON users FROM {runtime}")))
            .execute(&mut c)
            .await
            .unwrap();
    }
    // Tenant ruim: histórico de migrations adulterado.
    let mut c = w.env.migrator(&w.env.db_name(bad).await).await;
    sqlx::query("UPDATE _sqlx_migrations SET checksum = '\\x00'")
        .execute(&mut c)
        .await
        .unwrap();

    let err = migrate::run_with(&w.env.db, 2).await.unwrap_err();
    assert!(err.to_string().contains("1 de 3"), "{err}");
    for t in [w.tenant, good] {
        let mut rt = w.env.runtime(&w.env.db_name(t).await).await;
        sqlx::query("SELECT id FROM users LIMIT 1")
            .execute(&mut rt)
            .await
            .expect("grant reaplicado");
    }
    // Corrigido o tenant ruim, o migrate volta a passar.
    let mut g = w.env.migrator(&w.env.db_name(good).await).await;
    let checksum: Vec<u8> = sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
        .fetch_one(&mut g)
        .await
        .unwrap();
    sqlx::query("UPDATE _sqlx_migrations SET checksum = $1")
        .bind(checksum)
        .execute(&mut c)
        .await
        .unwrap();
    migrate::run_with(&w.env.db, 2).await.unwrap();
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn evicting_a_pool_does_not_break_in_flight_requests() {
    let w = World::new().await;
    let app = &w.app;
    assert_eq!(app.get("/v1/users", w.admin()).await.status, StatusCode::OK);
    let info = app.state.registry.by_id(w.tenant).await.unwrap().unwrap();
    let handle = app.state.pools.get(&info).await.unwrap();

    // Consulta longa em andamento.
    let pool = handle.clone();
    let inflight = tokio::spawn(async move { sqlx::query("SELECT pg_sleep(0.5)").execute(&pool.pool).await.map(drop) });
    tokio::time::sleep(Duration::from_millis(100)).await;
    app.state.pools.remove(w.tenant).await;
    app.state.pools.run_pending_tasks().await;
    drop(handle);

    // Novas requisições usam um pool novo; a antiga termina normalmente.
    assert_eq!(app.get("/v1/users", w.admin()).await.status, StatusCode::OK);
    inflight.await.unwrap().expect("consulta em andamento não pode falhar");

    // Capacidade: com 2 pools no máximo, abrir 4 tenants despeja pools sem erros.
    let mut cfg = w.env.api_config();
    cfg.max_open_tenant_pools = 2;
    let small = w.env.app_with(cfg).await;
    let owner = small.owner().await;
    let mut slugs = vec![SEED_SLUG.to_owned()];
    for i in 0..3 {
        let s = format!("pool-{i}");
        small.create_tenant(&w.env, &owner, &s).await;
        slugs.push(s);
    }
    for _ in 0..3 {
        for s in &slugs {
            assert_eq!(
                small.get("/v1/users", tok_slug(&owner, s)).await.status,
                StatusCode::OK,
                "{s}"
            );
        }
    }
    small.state.pools.run_pending_tasks().await;
    let (cached, _, _) = small.state.pools.connection_stats();
    assert!(cached <= 2, "pools em cache: {cached}");
    drop(small);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bootstrap_is_idempotent_and_validation_mode_works() {
    let env = Env::new().await;
    // Segunda execução não falha nem altera senhas (o runtime continua entrando).
    simple_core::cli::bootstrap::bootstrap(&env.db).await.unwrap();
    simple_core::cli::bootstrap::validate(&env.db).await.unwrap();
    let mut rt = env.runtime(&env.db.control_db).await;
    sqlx::query("SELECT 1").execute(&mut rt).await.unwrap();
    // migrate também é idempotente.
    migrate::run_with(&env.db, 2).await.unwrap();
    env.cleanup().await;
}
