//! Plataforma: papéis, owner único, superusuários e escopos de token.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;
use simple_core::app::route_defs;
use simple_core::auth::middleware::Access;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_owner_creates_tenants_superusers_and_admins() {
    let w = World::new().await;
    let app = &w.app;
    let (_, superuser) = app.create_superuser(&w.owner, "su@p.io").await;
    let (_, common) = app
        .user_with(w.admin(), SEED_SLUG, "common@seed.io", &["users.read"])
        .await;

    // Owner consegue.
    let t = app
        .post(
            "/v1/platform/tenants",
            tok(&w.owner),
            json!({ "slug": "by-owner", "name": "x" }),
        )
        .await;
    assert_eq!(t.status, StatusCode::ACCEPTED);

    for (who, token) in [
        ("superuser", &superuser),
        ("tenant admin", &w.admin),
        ("common", &common),
    ] {
        let r = app
            .post(
                "/v1/platform/tenants",
                tok(token),
                json!({ "slug": "nope", "name": "x" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} criou tenant");
        let r = app
            .post(
                "/v1/platform/superusers",
                tok(token),
                json!({ "email": "x@x.io", "name": "x", "password": PASSWORD }),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} criou superusuário");
        let r = app
            .post(
                &format!("/v1/platform/tenants/{}/admins", w.tenant),
                tok(token),
                json!({ "email": "y@y.io", "name": "y", "password": PASSWORD }),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} criou admin");
        let r = app
            .delete(
                &format!("/v1/platform/tenants/{}/admins/{}", w.tenant, w.admin_id),
                tok(token),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} removeu admin");
        let r = app
            .patch(
                &format!("/v1/platform/tenants/{}", w.tenant),
                tok(token),
                json!({ "status": "suspended" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{who} suspendeu tenant");
    }
    // Problem details.
    let r = app
        .post(
            "/v1/platform/tenants",
            tok(&superuser),
            json!({ "slug": "nope", "name": "x" }),
        )
        .await;
    assert_eq!(r.content_type.as_deref(), Some("application/problem+json"));
    assert_eq!(r.body["status"], 403);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn superuser_accesses_all_tenants_but_cannot_change_admin_flag() {
    let w = World::new().await;
    let app = &w.app;
    let other = app.create_tenant(&w.env, &w.owner, "other").await;
    app.create_admin(&w.owner, other, "admin@other.io").await;
    let (_, su) = app.create_superuser(&w.owner, "su@p.io").await;

    // Lê tenants e entra em qualquer tenant com X-Tenant-Slug.
    assert_eq!(app.get("/v1/platform/tenants", tok(&su)).await.status, StatusCode::OK);
    for slug in [SEED_SLUG, "other"] {
        let r = app.get("/v1/users", tok_slug(&su, slug)).await;
        assert_eq!(r.status, StatusCode::OK, "{slug}");
        assert_eq!(r.body["items"].as_array().unwrap().len(), 1);
        let u = app
            .post(
                "/v1/users",
                tok_slug(&su, slug),
                json!({ "email": "made@by.su", "name": "x", "password": PASSWORD }),
            )
            .await;
        assert_eq!(u.status, StatusCode::CREATED);
        // is_tenant_admin não é alterável pela API comum.
        let r = app
            .patch(
                &format!("/v1/users/{}", u.id()),
                tok_slug(&su, slug),
                json!({ "is_tenant_admin": true }),
            )
            .await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    }
    // Nem pelas rotas do owner.
    let r = app
        .delete(
            &format!("/v1/platform/tenants/{}/admins/{}", w.tenant, w.admin_id),
            tok(&su),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .get(&format!("/v1/platform/tenants/{}/admins", w.tenant), tok(&su))
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn nobody_creates_changes_or_deletes_the_owner_even_concurrently() {
    let w = World::new().await;
    let app = &w.app;
    let me = app.get("/v1/platform/me", tok(&w.owner)).await;
    let owner_id = me.id();
    let mut ctl = w.env.control().await;
    let hash_before: String = sqlx::query_scalar("SELECT password_hash FROM platform_users WHERE id = $1")
        .bind(owner_id)
        .fetch_one(&mut ctl)
        .await
        .unwrap();

    let mut tasks = Vec::new();
    for i in 0..10 {
        let app = app.clone();
        let owner = w.owner.clone();
        tasks.push(tokio::spawn(async move {
            let path = format!("/v1/platform/superusers/{owner_id}");
            let a = app
                .post(
                    "/v1/platform/superusers",
                    tok(&owner),
                    json!({ "email": OWNER_EMAIL, "name": "x", "password": PASSWORD }),
                )
                .await
                .status;
            let b = app
                .patch(
                    &path,
                    tok(&owner),
                    json!({ "is_active": false, "name": format!("x{i}") }),
                )
                .await
                .status;
            let c = app
                .patch(&path, tok(&owner), json!({ "password": "another-password-123" }))
                .await
                .status;
            let d = app.delete(&path, tok(&owner)).await.status;
            (a, b, c, d)
        }));
    }
    for t in tasks {
        let (a, b, c, d) = t.await.unwrap();
        assert_eq!(a, StatusCode::CONFLICT, "owner duplicado");
        assert_eq!(b, StatusCode::NOT_FOUND, "owner alterado");
        assert_eq!(c, StatusCode::NOT_FOUND, "senha do owner alterada");
        assert_eq!(d, StatusCode::NOT_FOUND, "owner excluído");
    }
    // Campo `role` não existe na API.
    let r = app
        .post(
            "/v1/platform/superusers",
            tok(&w.owner),
            json!({ "email": "o2@p.io", "name": "x", "password": PASSWORD, "role": "owner" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);

    let (n, active, hash): (i64, bool, String) =
        sqlx::query_as("SELECT count(*) OVER (), is_active, password_hash FROM platform_users WHERE role = 'owner'")
            .fetch_one(&mut ctl)
            .await
            .unwrap();
    assert_eq!(n, 1);
    assert!(active);
    assert_eq!(hash, hash_before);

    // Nem por SQL com app_runtime: o índice único impede o segundo owner.
    let mut rt = w.env.runtime(&w.env.db.control_db).await;
    let e = sqlx::query(
        "INSERT INTO platform_users (id, email, name, password_hash, role) VALUES ($1, 'o3@p.io', 'x', 'x', 'owner')",
    )
    .bind(Uuid::now_v7())
    .execute(&mut rt)
    .await
    .unwrap_err();
    assert_eq!(simple_core::db::pg_code(&e).as_deref(), Some("23505"));
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deactivated_superuser_loses_access_after_invalidation() {
    let w = World::new().await;
    let app2 = w.env.app().await;
    let (su_id, su) = w.app.create_superuser(&w.owner, "su@p.io").await;
    // Aquece o cache das duas instâncias.
    assert_eq!(w.app.get("/v1/platform/tenants", tok(&su)).await.status, StatusCode::OK);
    assert_eq!(app2.get("/v1/platform/tenants", tok(&su)).await.status, StatusCode::OK);
    assert_eq!(
        app2.get("/v1/users", tok_slug(&su, SEED_SLUG)).await.status,
        StatusCode::OK
    );

    let r = w
        .app
        .patch(
            &format!("/v1/platform/superusers/{su_id}"),
            tok(&w.owner),
            json!({ "is_active": false }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        w.app.get("/v1/platform/tenants", tok(&su)).await.status,
        StatusCode::UNAUTHORIZED
    );
    // A outra instância recebe o NOTIFY do trigger.
    eventually("invalidação na instância 2", || async {
        app2.get("/v1/platform/tenants", tok(&su)).await.status == StatusCode::UNAUTHORIZED
    })
    .await;
    assert_eq!(
        app2.get("/v1/users", tok_slug(&su, SEED_SLUG)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = w
        .app
        .post(
            "/v1/platform/auth/login",
            NONE,
            json!({ "email": "su@p.io", "password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    drop(app2);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_scopes_are_separated() {
    let w = World::new().await;
    let app = &w.app;
    // Token de tenant em rota de plataforma.
    for path in ["/v1/platform/me", "/v1/platform/tenants"] {
        assert_eq!(app.get(path, w.admin()).await.status, StatusCode::FORBIDDEN, "{path}");
    }
    // Token de plataforma sem X-Tenant-Slug em rota de tenant.
    assert_eq!(app.get("/v1/users", tok(&w.owner)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(
        app.get("/v1/users", tok_slug(&w.owner, SEED_SLUG)).await.status,
        StatusCode::OK
    );
    // /v1/me é exclusivo de token de tenant.
    assert_eq!(
        app.get("/v1/me", tok_slug(&w.owner, SEED_SLUG)).await.status,
        StatusCode::FORBIDDEN
    );
    let me = app.get("/v1/me", w.admin()).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["tenant"]["slug"], SEED_SLUG);
    // Token inválido / assinado com outro segredo.
    assert_eq!(
        app.get("/v1/users", tok("abc.def.ghi")).await.status,
        StatusCode::UNAUTHORIZED
    );
    w.cleanup().await;
}

/// Percorre todas as rotas: nenhuma rota não pública pode responder sem
/// autenticação, e todas as rotas de feature negam usuário sem features.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_non_public_route_requires_auth_and_access_rule() {
    let w = World::new().await;
    let app = &w.app;
    let (_, nobody) = app.user_with(w.admin(), SEED_SLUG, "nobody@seed.io", &[]).await;
    let (_, su) = app.create_superuser(&w.owner, "su@p.io").await;
    let other = Uuid::now_v7();

    let defs = route_defs();
    assert!(defs.len() >= 35, "rotas esperadas: {}", defs.len());
    let public: Vec<_> = defs.iter().filter(|d| d.access.is_public()).map(|d| d.path).collect();
    assert_eq!(
        public,
        [
            "/health/live",
            "/health/ready",
            "/v1/auth/login",
            "/v1/auth/refresh",
            "/v1/platform/auth/login",
            "/v1/platform/auth/refresh"
        ]
    );

    for d in &defs {
        let path = d
            .path
            .replace("{id}", &other.to_string())
            .replace("{user_id}", &other.to_string())
            .replace("{feature_code}", "users.read");
        let body = matches!(d.method, Method::POST | Method::PUT | Method::PATCH).then(|| json!({}));
        if d.access.is_public() {
            continue;
        }
        let r = app.call(d.method.clone(), &path, NONE, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{} {} sem token", d.method, d.path);

        match d.access {
            Access::Feature(_) => {
                let r = app.call(d.method.clone(), &path, tok(&nobody), body.clone()).await;
                assert_eq!(r.status, StatusCode::FORBIDDEN, "{} {} sem feature", d.method, d.path);
            }
            Access::PlatformOwner => {
                let r = app.call(d.method.clone(), &path, tok(&su), body.clone()).await;
                assert_eq!(
                    r.status,
                    StatusCode::FORBIDDEN,
                    "{} {} com superusuário",
                    d.method,
                    d.path
                );
            }
            Access::Platform => {
                let r = app.call(d.method.clone(), &path, tok(&nobody), body.clone()).await;
                assert_eq!(
                    r.status,
                    StatusCode::FORBIDDEN,
                    "{} {} com token de tenant",
                    d.method,
                    d.path
                );
            }
            Access::SelfOr(_) => {
                let r = app.call(d.method.clone(), &path, tok(&nobody), body.clone()).await;
                assert_eq!(
                    r.status,
                    StatusCode::FORBIDDEN,
                    "{} {} em outro usuário",
                    d.method,
                    d.path
                );
            }
            Access::TenantToken | Access::Public => {}
        }
    }
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn platform_login_is_rate_limited_more_strictly() {
    let env = Env::new().await;
    env.seed().await.unwrap();
    let mut cfg = env.api_config();
    cfg.login_limit_platform = simple_core::config::RateLimit {
        per_ip: 20,
        per_email: 3,
    };
    cfg.login_limit_tenant = simple_core::config::RateLimit {
        per_ip: 60,
        per_email: 10,
    };
    let app = env.app_with(cfg).await;
    let mut statuses = Vec::new();
    for _ in 0..5 {
        statuses.push(
            app.post(
                "/v1/platform/auth/login",
                NONE,
                json!({ "email": OWNER_EMAIL, "password": "wrong-password-xx" }),
            )
            .await
            .status,
        );
    }
    assert_eq!(statuses[..3], [StatusCode::UNAUTHORIZED; 3]);
    assert_eq!(statuses[3..], [StatusCode::TOO_MANY_REQUESTS; 2]);
    // Mesmo e-mail num tenant tem orçamento próprio.
    let r = app.tenant_login_raw(SEED_SLUG, OWNER_EMAIL, "wrong-password-xx").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    drop(app);
    env.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn platform_me_and_superuser_management() {
    let w = World::new().await;
    let app = &w.app;
    let (su_id, su) = app.create_superuser(&w.owner, "su@p.io").await;
    // Troca de senha exige current_password.
    let r = app
        .patch("/v1/platform/me", tok(&su), json!({ "password": "new-password-12345" }))
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = app
        .patch(
            "/v1/platform/me",
            tok(&su),
            json!({ "password": "new-password-12345", "current_password": "wrong-one-xxxx" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .patch(
            "/v1/platform/me",
            tok(&su),
            json!({ "name": "Novo", "password": "new-password-12345", "current_password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["name"], "Novo");
    app.platform_login("su@p.io", "new-password-12345").await;
    // Listagem e exclusão pelo owner.
    let l = app.get("/v1/platform/superusers", tok(&w.owner)).await;
    assert_eq!(l.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        app.delete(&format!("/v1/platform/superusers/{su_id}"), tok(&w.owner))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.delete(&format!("/v1/platform/superusers/{su_id}"), tok(&w.owner))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    // Auditoria na mesma transação.
    let mut ctl = w.env.control().await;
    let actions: Vec<String> = sqlx::query_scalar("SELECT action FROM platform_audit ORDER BY id")
        .fetch_all(&mut ctl)
        .await
        .unwrap();
    for a in ["superuser.create", "platform_user.update_self", "superuser.delete"] {
        assert!(actions.iter().any(|x| x == a), "{a} ausente em {actions:?}");
    }
    w.cleanup().await;
}
