//! Isolamento entre tenants e privilégios das roles do PostgreSQL.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use sqlx::PgConnection;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_email_in_two_tenants_is_independent() {
    let w = World::new().await;
    let app = &w.app;
    let b = app.create_tenant(&w.env, &w.owner, "beta").await;
    app.create_admin(&w.owner, b, "admin@beta.io").await;
    let admin_b = app.tenant_login("beta", "admin@beta.io").await.0;

    // Mesmo e-mail, senhas e permissões diferentes.
    let r = app
        .post(
            "/v1/users",
            w.admin(),
            json!({ "email": "same@x.io", "name": "A", "password": "senha-do-tenant-a" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let id_a = r.id();
    let r = app
        .post(
            "/v1/users",
            tok(&admin_b),
            json!({ "email": "same@x.io", "name": "B", "password": "senha-do-tenant-b" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let p = app.create_profile(w.admin(), "readers", &["users.read"]).await;
    assert_eq!(app.set_profiles(w.admin(), id_a, &[p]).await.status, StatusCode::OK);

    assert_eq!(
        app.tenant_login_raw(SEED_SLUG, "same@x.io", "senha-do-tenant-b")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.tenant_login_raw("beta", "same@x.io", "senha-do-tenant-a")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    let ta = app.tenant_login_raw(SEED_SLUG, "same@x.io", "senha-do-tenant-a").await;
    let tb = app.tenant_login_raw("beta", "same@x.io", "senha-do-tenant-b").await;
    let (ta, tb) = (
        ta.body["access_token"].as_str().unwrap(),
        tb.body["access_token"].as_str().unwrap(),
    );
    assert_eq!(app.get("/v1/users", tok(ta)).await.status, StatusCode::OK);
    assert_eq!(app.get("/v1/users", tok(tb)).await.status, StatusCode::FORBIDDEN);
    // A mesma conta na plataforma também é independente.
    let r = app
        .post(
            "/v1/platform/superusers",
            tok(&w.owner),
            json!({ "email": "same@x.io", "name": "P", "password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_a_token_never_reaches_tenant_b() {
    let w = World::new().await;
    let app = &w.app;
    let b = app.create_tenant(&w.env, &w.owner, "beta").await;
    let admin_b_id = app.create_admin(&w.owner, b, "admin@beta.io").await;
    let admin_b = app.tenant_login("beta", "admin@beta.io").await.0;
    let user_b = app.create_user(tok(&admin_b), "only-in-b@x.io").await;

    // Header de outro tenant com token do tenant A: 403.
    assert_eq!(
        app.get("/v1/users", tok_slug(&w.admin, "beta")).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get(&format!("/v1/users/{user_b}"), tok_slug(&w.admin, "beta"))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    // Mesmo header correto: os dados de B não existem no banco de A.
    assert_eq!(
        app.get(&format!("/v1/users/{user_b}"), w.admin()).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.delete(&format!("/v1/users/{admin_b_id}"), w.admin()).await.status,
        StatusCode::NOT_FOUND
    );
    let list = app.get("/v1/users", w.admin()).await;
    assert!(!list.body.to_string().contains("only-in-b@x.io"));
    // Admin de tenant restrito ao próprio tenant e fora da plataforma.
    assert_eq!(
        app.get("/v1/users", tok_slug(&admin_b, SEED_SLUG)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.get("/v1/platform/tenants", tok(&admin_b)).await.status,
        StatusCode::FORBIDDEN
    );
    let r = app
        .post(
            &format!("/v1/platform/tenants/{b}/admins"),
            tok(&admin_b),
            json!({ "email": "x@x.io", "name": "x", "password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Refresh token de A não serve em B.
    let (_, refresh_a) = app.tenant_login(SEED_SLUG, "admin@seed.io").await;
    let r = app
        .call(
            axum::http::Method::POST,
            "/v1/auth/refresh",
            Auth {
                token: None,
                slug: Some("beta"),
            },
            Some(json!({ "refresh_token": refresh_a })),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn suspended_tenant_is_blocked_quickly_and_reactivated() {
    let w = World::new().await;
    let app = &w.app;
    let app2 = w.env.app().await;
    // Caches quentes nas duas instâncias.
    assert_eq!(app.get("/v1/users", w.admin()).await.status, StatusCode::OK);
    assert_eq!(app2.get("/v1/users", w.admin()).await.status, StatusCode::OK);

    let path = format!("/v1/platform/tenants/{}", w.tenant);
    let r = app.patch(&path, tok(&w.owner), json!({ "status": "suspended" })).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(app.get("/v1/users", w.admin()).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        app.get("/v1/users", tok_slug(&w.owner, SEED_SLUG)).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.tenant_login_raw(SEED_SLUG, "admin@seed.io", PASSWORD).await.status,
        StatusCode::FORBIDDEN
    );
    eventually("suspensão na instância 2", || async {
        app2.get("/v1/users", w.admin()).await.status == StatusCode::FORBIDDEN
    })
    .await;
    // Só active <-> suspended.
    let r = app
        .patch(&path, tok(&w.owner), json!({ "status": "provisioning" }))
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    let r = app.patch(&path, tok(&w.owner), json!({ "status": "active" })).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(app.get("/v1/users", w.admin()).await.status, StatusCode::OK);
    eventually("reativação na instância 2", || async {
        app2.get("/v1/users", w.admin()).await.status == StatusCode::OK
    })
    .await;
    assert_eq!(
        app.tenant_login_raw(SEED_SLUG, "admin@seed.io", PASSWORD).await.status,
        StatusCode::OK
    );
    drop(app2);
    w.cleanup().await;
}

async fn denied(conn: &mut PgConnection, sql: &str) {
    let e = sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned())).execute(conn).await;
    let code = e.as_ref().err().and_then(simple_core::db::pg_code);
    assert_eq!(code.as_deref(), Some("42501"), "deveria ser negado: {sql} -> {e:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn runtime_role_cannot_ddl_create_databases_or_tamper() {
    let w = World::new().await;
    let control = w.env.db.control_db.clone();
    let mut rt = w.env.runtime(&control).await;
    denied(&mut rt, "CREATE TABLE evil (id int)").await;
    denied(&mut rt, "CREATE DATABASE evil").await;
    denied(
        &mut rt,
        "UPDATE tenants SET db_name = 'tenant_00000000000000000000000000000000'",
    )
    .await;
    denied(&mut rt, "UPDATE tenants SET slug = 'x'").await;
    denied(&mut rt, "UPDATE platform_users SET role = 'owner'").await;
    denied(&mut rt, "UPDATE platform_audit SET action = 'x'").await;
    denied(&mut rt, "DELETE FROM platform_audit").await;
    denied(&mut rt, "TRUNCATE platform_audit").await;
    denied(&mut rt, "DROP TABLE tenants").await;
    denied(&mut rt, "ALTER TABLE tenants ADD COLUMN x int").await;
    let attrs: (bool, bool) = sqlx::query_as("SELECT rolcreatedb, rolsuper FROM pg_roles WHERE rolname = current_user")
        .fetch_one(&mut rt)
        .await
        .unwrap();
    assert_eq!(attrs, (false, false));

    let db = w.env.db_name(w.tenant).await;
    let mut rt = w.env.runtime(&db).await;
    denied(&mut rt, "CREATE TABLE evil (id int)").await;
    denied(&mut rt, "UPDATE access_audit SET action = 'x'").await;
    denied(&mut rt, "DELETE FROM access_audit").await;
    denied(
        &mut rt,
        "INSERT INTO features (module_id, code, name) VALUES (1, 'x.y', 'x')",
    )
    .await;
    denied(&mut rt, "DELETE FROM modules").await;
    denied(&mut rt, "DELETE FROM _sqlx_migrations").await;
    denied(&mut rt, "CREATE TEMP TABLE t (id int)").await;
    // DML permitido.
    sqlx::query("SELECT count(*) FROM users")
        .execute(&mut rt)
        .await
        .unwrap();
    let audits: i64 = sqlx::query_scalar("SELECT count(*) FROM access_audit")
        .fetch_one(&mut rt)
        .await
        .unwrap();
    assert!(audits >= 1, "criação do admin deve estar auditada");

    // CONNECT revogado de PUBLIC e concedido só ao app_runtime (o dono é o migrator).
    let mut admin = w.env.admin(&w.env.db.admin_db).await;
    let acl: String = sqlx::query_scalar("SELECT datacl::text FROM pg_database WHERE datname = $1")
        .bind(&db)
        .fetch_one(&mut admin)
        .await
        .unwrap();
    // Entradas de PUBLIC no aclitem começam com '='.
    let entries: Vec<&str> = acl.trim_matches(|c| c == '{' || c == '}').split(',').collect();
    assert!(
        !entries.iter().any(|e| e.starts_with('=')),
        "PUBLIC não pode ter privilégios: {acl}"
    );
    assert!(acl.contains(&format!("{}=c/", w.env.db.runtime_user)), "{acl}");
    w.cleanup().await;
}
