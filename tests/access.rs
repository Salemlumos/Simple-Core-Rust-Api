//! CRUD de usuários, resolução de permissões, cache e proteções contra escalada.

mod common;

use axum::http::{Method, StatusCode};
use common::*;
use serde_json::json;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn users_crud_with_404_409_422_and_concurrent_duplicates() {
    let w = World::new().await;
    let app = &w.app;
    let a = w.admin();

    // 201 + normalização de e-mail.
    let r = app
        .post(
            "/v1/users",
            a,
            json!({ "email": "  Ana@Example.COM ", "name": "Ana", "password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.body["email"], "ana@example.com");
    assert!(r.body.get("password_hash").is_none() && r.body.get("password").is_none());
    let id = r.id();
    assert_eq!(app.get(&format!("/v1/users/{id}"), a).await.body["name"], "Ana");

    // 409 em e-mail duplicado.
    let r = app
        .post(
            "/v1/users",
            a,
            json!({ "email": "ana@example.com", "name": "x", "password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    // 422: campos inválidos e desconhecidos.
    for body in [
        json!({ "email": "sem-arroba", "name": "x", "password": PASSWORD }),
        json!({ "email": "a@b.io", "name": "", "password": PASSWORD }),
        json!({ "email": "a@b.io", "name": "x", "password": "curta" }),
        json!({ "email": "a@b.io", "name": "x", "password": "x".repeat(129) }),
        json!({ "email": "a@b.io", "name": "x", "password": PASSWORD, "is_tenant_admin": true }),
        json!({ "email": "a@b.io", "name": "x" }),
    ] {
        let r = app.post("/v1/users", a, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(r.content_type.as_deref(), Some("application/problem+json"));
    }
    // Body acima de 16 KB.
    let big = json!({ "email": "a@b.io", "name": "x".repeat(17 * 1024), "password": PASSWORD });
    assert_eq!(
        app.post("/v1/users", a, big).await.status,
        StatusCode::PAYLOAD_TOO_LARGE
    );

    // PATCH, 404 e DELETE.
    let r = app
        .patch(
            &format!("/v1/users/{id}"),
            a,
            json!({ "name": "Ana Maria", "email": "ANA2@x.io" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body["email"], "ana2@x.io");
    let missing = Uuid::now_v7();
    assert_eq!(
        app.get(&format!("/v1/users/{missing}"), a).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.patch(&format!("/v1/users/{missing}"), a, json!({ "name": "x" }))
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        app.delete(&format!("/v1/users/{missing}"), a).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(app.get("/v1/users/not-a-uuid", a).await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        app.delete(&format!("/v1/users/{id}"), a).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.get(&format!("/v1/users/{id}"), a).await.status,
        StatusCode::NOT_FOUND
    );

    // E-mails duplicados concorrentes: exatamente um vence.
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let app = app.clone();
        let t = w.admin.clone();
        tasks.push(tokio::spawn(async move {
            app.post(
                "/v1/users",
                tok(&t),
                json!({ "email": "race@x.io", "name": "r", "password": PASSWORD }),
            )
            .await
            .status
        }));
    }
    let mut created = 0;
    for t in tasks {
        match t.await.unwrap() {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => {}
            other => panic!("status inesperado {other}"),
        }
    }
    assert_eq!(created, 1);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keyset_pagination() {
    let w = World::new().await;
    let app = &w.app;
    for i in 0..5 {
        app.create_user(w.admin(), &format!("u{i}@x.io")).await;
    }
    // 6 usuários (admin + 5), páginas de 4.
    let p1 = app.get("/v1/users?limit=4", w.admin()).await;
    assert_eq!(p1.body["items"].as_array().unwrap().len(), 4);
    let after = p1.body["next_after"].as_str().unwrap().to_owned();
    let p2 = app.get(&format!("/v1/users?limit=4&after={after}"), w.admin()).await;
    assert_eq!(p2.body["items"].as_array().unwrap().len(), 2);
    assert!(p2.body["next_after"].is_null());
    let ids: Vec<_> = [&p1, &p2]
        .iter()
        .flat_map(|p| {
            p.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|u| u["id"].as_str().unwrap().to_owned())
        })
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "ordenado por id");
    assert_eq!(
        app.get("/v1/users?limit=101", w.admin()).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        app.get("/v1/users?limit=0", w.admin()).await.status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn permissions_from_profiles_union_and_allow_override() {
    let w = World::new().await;
    let app = &w.app;
    let (uid, t) = app.user_with(w.admin(), SEED_SLUG, "u@x.io", &["users.read"]).await;
    assert_eq!(app.get("/v1/users", tok(&t)).await.status, StatusCode::OK);
    assert_eq!(app.create_user_status(&t, "n1@x.io").await, StatusCode::FORBIDDEN);

    // Soma de múltiplos perfis.
    let p_read = app.get(&format!("/v1/users/{uid}/access"), w.admin()).await.body["profiles"][0]["id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    let p_create = app.create_profile(w.admin(), "creators", &["users.create"]).await;
    assert_eq!(
        app.set_profiles(w.admin(), uid, &[p_read, p_create]).await.status,
        StatusCode::OK
    );
    assert_eq!(app.create_user_status(&t, "n2@x.io").await, StatusCode::CREATED);
    assert_eq!(app.get("/v1/users", tok(&t)).await.status, StatusCode::OK);

    // Exceção allow.
    let victim = app.create_user(w.admin(), "victim@x.io").await;
    assert_eq!(
        app.delete(&format!("/v1/users/{victim}"), tok(&t)).await.status,
        StatusCode::FORBIDDEN
    );
    let r = app
        .put(
            &format!("/v1/users/{uid}/overrides/users.delete"),
            w.admin(),
            json!({ "effect": "allow", "reason": "suporte" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{r:?}");
    assert_eq!(
        app.delete(&format!("/v1/users/{victim}"), tok(&t)).await.status,
        StatusCode::NO_CONTENT
    );

    let acc = app.get(&format!("/v1/users/{uid}/access"), w.admin()).await;
    assert_eq!(
        acc.body["effective_features"],
        json!(["users.read", "users.create", "users.delete"])
    );
    // Exceção sem motivo é rejeitada.
    let r = app
        .put(
            &format!("/v1/users/{uid}/overrides/users.update"),
            w.admin(),
            json!({ "effect": "allow", "reason": " " }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deny_wins_and_expired_overrides_are_ignored() {
    let w = World::new().await;
    let app = &w.app;
    let (uid, t) = app
        .user_with(w.admin(), SEED_SLUG, "u@x.io", &["users.read", "users.create"])
        .await;
    let path = format!("/v1/users/{uid}/overrides/users.read");

    // deny vence o perfil.
    let r = app
        .put(&path, w.admin(), json!({ "effect": "deny", "reason": "auditoria" }))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(app.get("/v1/users", tok(&t)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.create_user_status(&t, "still@x.io").await, StatusCode::CREATED);

    // deny vence perfil + allow: com dois perfis concedendo, segue negado.
    let p2 = app.create_profile(w.admin(), "readers-2", &["users.read"]).await;
    let current = app.get(&format!("/v1/users/{uid}/access"), w.admin()).await.body["profiles"][0]["id"]
        .as_str()
        .unwrap()
        .parse::<Uuid>()
        .unwrap();
    app.set_profiles(w.admin(), uid, &[current, p2]).await;
    assert_eq!(app.get("/v1/users", tok(&t)).await.status, StatusCode::FORBIDDEN);

    // allow + deny: deny em users.create vence o allow de outra fonte (perfil) e,
    // ao trocar deny por allow, volta a valer.
    let r = app
        .put(&path, w.admin(), json!({ "effect": "allow", "reason": "liberado" }))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(app.get("/v1/users", tok(&t)).await.status, StatusCode::OK);

    // Exceção expirada é ignorada (inserida direto no banco; a API exige data futura).
    let db = w.env.db_name(w.tenant).await;
    let mut conn = w.env.migrator(&db).await;
    sqlx::query(
        "UPDATE user_feature_overrides SET effect = 'deny', expires_at = now() - interval '1 minute'
         WHERE user_id = $1",
    )
    .bind(uid)
    .execute(&mut conn)
    .await
    .unwrap();
    w.app.state.access.invalidate_user(w.tenant, uid).await;
    assert_eq!(
        app.get("/v1/users", tok(&t)).await.status,
        StatusCode::OK,
        "deny expirado não vale"
    );
    // allow expirado também não concede.
    sqlx::query(
        "INSERT INTO user_feature_overrides (user_id, feature_id, effect, reason, expires_at, created_by, created_by_type)
         SELECT $1, id, 'allow', 'old', now() - interval '1 minute', $1, 'tenant' FROM features WHERE code = 'users.delete'",
    )
    .bind(uid)
    .execute(&mut conn)
    .await
    .unwrap();
    w.app.state.access.invalidate_user(w.tenant, uid).await;
    let other = app.create_user(w.admin(), "o@x.io").await;
    assert_eq!(
        app.delete(&format!("/v1/users/{other}"), tok(&t)).await.status,
        StatusCode::FORBIDDEN
    );
    // Data no passado é rejeitada pela API.
    let r = app
        .put(
            &path,
            w.admin(),
            json!({ "effect": "deny", "reason": "x", "expires_at": "2000-01-01T00:00:00Z" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inactive_user_401_and_missing_feature_403() {
    let w = World::new().await;
    let app = &w.app;
    let (uid, t) = app.user_with(w.admin(), SEED_SLUG, "u@x.io", &["users.read"]).await;
    assert_eq!(app.get("/v1/users", tok(&t)).await.status, StatusCode::OK);
    assert_eq!(app.get("/v1/modules", tok(&t)).await.status, StatusCode::FORBIDDEN);
    assert_eq!(app.get("/v1/users", NONE).await.status, StatusCode::UNAUTHORIZED);

    let r = app
        .patch(&format!("/v1/users/{uid}"), w.admin(), json!({ "is_active": false }))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(app.get("/v1/users", tok(&t)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(app.get("/v1/me", tok(&t)).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.tenant_login_raw(SEED_SLUG, "u@x.io", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    // Mesmo 401 para senha errada, e-mail inexistente e tenant inexistente.
    assert_eq!(
        app.tenant_login_raw(SEED_SLUG, "admin@seed.io", "wrong-password")
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.tenant_login_raw(SEED_SLUG, "ghost@x.io", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.tenant_login_raw("no-such-tenant", "admin@seed.io", PASSWORD)
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn self_service_name_and_password() {
    let w = World::new().await;
    let app = &w.app;
    let (uid, t) = app.user_with(w.admin(), SEED_SLUG, "me@x.io", &[]).await;
    let me = format!("/v1/users/{uid}");
    assert_eq!(
        app.patch(&me, tok(&t), json!({ "name": "Eu" })).await.status,
        StatusCode::OK
    );
    assert_eq!(
        app.patch(&me, tok(&t), json!({ "email": "new@x.io" })).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.patch(&me, tok(&t), json!({ "is_active": false })).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        app.patch(&me, tok(&t), json!({ "password": "nova-senha-123456" }))
            .await
            .status,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        app.patch(
            &me,
            tok(&t),
            json!({ "password": "nova-senha-123456", "current_password": "errada-123456" })
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let r = app
        .patch(
            &me,
            tok(&t),
            json!({ "password": "nova-senha-123456", "current_password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        app.tenant_login_raw(SEED_SLUG, "me@x.io", "nova-senha-123456")
            .await
            .status,
        StatusCode::OK
    );
    // Outro usuário sem feature: 403.
    assert_eq!(
        app.patch(&format!("/v1/users/{}", w.admin_id), tok(&t), json!({ "name": "x" }))
            .await
            .status,
        StatusCode::FORBIDDEN
    );
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cache_invalidation_between_instances_hits_only_the_right_tenant() {
    let w = World::new().await;
    let app1 = &w.app;
    let app2 = w.env.app().await;
    let b = app1.create_tenant(&w.env, &w.owner, "beta").await;
    app1.create_admin(&w.owner, b, "admin@beta.io").await;
    let admin_b = app1.tenant_login("beta", "admin@beta.io").await.0;
    let (uid_b, tb) = app1.user_with(tok(&admin_b), "beta", "ub@x.io", &["users.read"]).await;
    let (uid, t) = app1.user_with(w.admin(), SEED_SLUG, "ua@x.io", &["users.read"]).await;

    // Instância 2 com caches quentes nos dois tenants.
    assert_eq!(app2.get("/v1/users", tok(&t)).await.status, StatusCode::OK);
    assert_eq!(app2.get("/v1/users", tok(&tb)).await.status, StatusCode::OK);
    assert!(app2.state.access.contains(w.tenant, uid));
    assert!(app2.state.access.contains(b, uid_b));

    // Exceção deny na instância 1 → instância 2 invalida o usuário.
    let r = app1
        .put(
            &format!("/v1/users/{uid}/overrides/users.read"),
            w.admin(),
            json!({ "effect": "deny", "reason": "x" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    eventually("deny propagado", || async {
        app2.get("/v1/users", tok(&t)).await.status == StatusCode::FORBIDDEN
    })
    .await;
    assert!(app2.state.access.contains(b, uid_b), "tenant B não pode ser afetado");

    // Alteração de perfil no tenant A invalida todo o tenant A, e só ele.
    assert_eq!(app2.get("/v1/users", w.admin()).await.status, StatusCode::OK);
    assert!(app2.state.access.contains(w.tenant, w.admin_id));
    let p = app1.create_profile(w.admin(), "extra", &["users.read"]).await;
    let r = app1
        .put(
            &format!("/v1/profiles/{p}/features"),
            w.admin(),
            json!({ "features": ["users.create"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    eventually("perfil propagado", || async {
        !app2.state.access.contains(w.tenant, w.admin_id)
    })
    .await;
    assert!(app2.state.access.contains(b, uid_b), "tenant B não pode ser afetado");
    assert_eq!(app2.get("/v1/users", tok(&tb)).await.status, StatusCode::OK);
    drop(app2);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn escalation_protections() {
    let w = World::new().await;
    let app = &w.app;
    // Gestor com access.users.manage + users.read, sem users.delete.
    let (mid, m) = app
        .user_with(
            w.admin(),
            SEED_SLUG,
            "mgr@x.io",
            &["access.users.manage", "access.profiles.manage", "users.read"],
        )
        .await;
    let target = app.create_user(w.admin(), "t@x.io").await;

    // Só concede o que possui (exceção).
    let r = app
        .put(
            &format!("/v1/users/{target}/overrides/users.delete"),
            tok(&m),
            json!({ "effect": "allow", "reason": "x" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = app
        .put(
            &format!("/v1/users/{target}/overrides/users.read"),
            tok(&m),
            json!({ "effect": "allow", "reason": "x" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // ... e via perfil.
    let r = app
        .post(
            "/v1/profiles",
            tok(&m),
            json!({ "name": "power", "features": ["users.delete"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let admin_profile = app.create_profile(w.admin(), "deleters", &["users.delete"]).await;
    assert_eq!(
        app.set_profiles(tok(&m), target, &[admin_profile]).await.status,
        StatusCode::FORBIDDEN
    );
    let r = app
        .put(
            &format!("/v1/profiles/{admin_profile}/features"),
            tok(&m),
            json!({ "features": ["users.delete", "users.update"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    // Remover deny equivale a conceder.
    let r = app
        .put(
            &format!("/v1/users/{target}/overrides/users.delete"),
            w.admin(),
            json!({ "effect": "deny", "reason": "x" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        app.delete(&format!("/v1/users/{target}/overrides/users.delete"), tok(&m))
            .await
            .status,
        StatusCode::FORBIDDEN
    );

    // Usuário comum não altera os próprios perfis/exceções.
    assert_eq!(app.set_profiles(tok(&m), mid, &[]).await.status, StatusCode::FORBIDDEN);
    let r = app
        .put(
            &format!("/v1/users/{mid}/overrides/users.read"),
            tok(&m),
            json!({ "effect": "allow", "reason": "x" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Admin só é alterado por usuário de plataforma.
    let admin2 = app.create_admin(&w.owner, w.tenant, "admin2@seed.io").await;
    for (m_, path, body) in [
        (
            Method::PATCH,
            format!("/v1/users/{admin2}"),
            Some(json!({ "name": "x" })),
        ),
        (
            Method::PATCH,
            format!("/v1/users/{admin2}"),
            Some(json!({ "is_active": false })),
        ),
        (Method::DELETE, format!("/v1/users/{admin2}"), None),
        (
            Method::PUT,
            format!("/v1/users/{admin2}/profiles"),
            Some(json!({ "profile_ids": [] })),
        ),
    ] {
        let r = app.call(m_.clone(), &path, w.admin(), body).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m_} {path}");
    }
    let r = app
        .patch(
            &format!("/v1/users/{admin2}"),
            tok_slug(&w.owner, SEED_SLUG),
            json!({ "name": "pelo owner" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    // Admin altera o próprio nome.
    assert_eq!(
        app.patch(&format!("/v1/users/{}", w.admin_id), w.admin(), json!({ "name": "eu" }))
            .await
            .status,
        StatusCode::OK
    );

    // Admin de tenant/usuário comum não acessam plataforma nem criam admins.
    let r = app
        .post(
            &format!("/v1/platform/tenants/{}/admins", w.tenant),
            w.admin(),
            json!({ "email": "z@z.io", "name": "z", "password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn last_active_admin_cannot_be_removed_even_concurrently() {
    let w = World::new().await;
    let app = &w.app;
    let su = app.create_superuser(&w.owner, "su@p.io").await.1;
    let pu = tok_slug(&su, SEED_SLUG);
    // Único admin: não pode ser desativado nem excluído pela API comum.
    let r = app
        .patch(&format!("/v1/users/{}", w.admin_id), pu, json!({ "is_active": false }))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(
        app.delete(&format!("/v1/users/{}", w.admin_id), pu).await.status,
        StatusCode::CONFLICT
    );

    // Dois admins removidos em paralelo: só um pode sair.
    for round in 0..5 {
        let a2 = app.create_admin(&w.owner, w.tenant, &format!("a{round}@seed.io")).await;
        let (x, y) = (w.admin_id, a2);
        let (app1, app2) = (app.clone(), app.clone());
        let (s1, s2) = (su.clone(), su.clone());
        let t1 = tokio::spawn(async move {
            app1.patch(
                &format!("/v1/users/{x}"),
                tok_slug(&s1, SEED_SLUG),
                json!({ "is_active": false }),
            )
            .await
            .status
        });
        let t2 = tokio::spawn(async move {
            app2.delete(&format!("/v1/users/{y}"), tok_slug(&s2, SEED_SLUG))
                .await
                .status
        });
        let (r1, r2) = (t1.await.unwrap(), t2.await.unwrap());
        let ok = [r1, r2].iter().filter(|s| s.is_success()).count();
        assert_eq!(ok, 1, "round {round}: {r1} {r2}");
        assert!([r1, r2].contains(&StatusCode::CONFLICT));
        // Restaura o admin original se ele foi desativado.
        if r1.is_success() {
            let r = app
                .patch(&format!("/v1/users/{x}"), pu, json!({ "is_active": true }))
                .await;
            assert_eq!(r.status, StatusCode::OK);
            app.delete(&format!("/v1/platform/tenants/{}/admins/{y}", w.tenant), tok(&w.owner))
                .await;
        }
    }
    // O owner pode remover o status de admin pelo endpoint exclusivo.
    let r = app
        .delete(
            &format!("/v1/platform/tenants/{}/admins/{}", w.tenant, w.admin_id),
            tok(&w.owner),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let me = app.get("/v1/me", w.admin()).await;
    assert_eq!(me.body["is_tenant_admin"], false);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_token_rotation_and_reuse_detection() {
    let w = World::new().await;
    let app = &w.app;
    let refresh = |rt: String| {
        let app = app.clone();
        async move {
            app.call(
                Method::POST,
                "/v1/auth/refresh",
                Auth {
                    token: None,
                    slug: Some(SEED_SLUG),
                },
                Some(json!({ "refresh_token": rt })),
            )
            .await
        }
    };
    let (_, r0) = app.tenant_login(SEED_SLUG, "admin@seed.io").await;
    let (_, other_session) = app.tenant_login(SEED_SLUG, "admin@seed.io").await;
    let a = refresh(r0.clone()).await;
    assert_eq!(a.status, StatusCode::OK);
    let r1 = a.body["refresh_token"].as_str().unwrap().to_owned();
    assert_ne!(r1, r0, "rotacionado");
    assert_eq!(
        app.get("/v1/me", tok(a.body["access_token"].as_str().unwrap()))
            .await
            .status,
        StatusCode::OK
    );

    // Reuso do token revogado: 401 e todas as sessões caem.
    assert_eq!(refresh(r0).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refresh(r1).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(refresh(other_session).await.status, StatusCode::UNAUTHORIZED);
    // Sem header de tenant.
    let r = app
        .post("/v1/auth/refresh", NONE, json!({ "refresh_token": "x" }))
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Logout revoga o refresh.
    let (at, rt) = app.tenant_login(SEED_SLUG, "admin@seed.io").await;
    assert_eq!(
        app.post("/v1/auth/logout", tok(&at), json!({ "refresh_token": rt }))
            .await
            .status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(refresh(rt).await.status, StatusCode::UNAUTHORIZED);

    // Plataforma: mesma regra.
    let (_, p0) = app.platform_login(OWNER_EMAIL, OWNER_PASSWORD).await;
    let p = app
        .post("/v1/platform/auth/refresh", NONE, json!({ "refresh_token": p0 }))
        .await;
    assert_eq!(p.status, StatusCode::OK);
    let p1 = p.body["refresh_token"].as_str().unwrap().to_owned();
    assert_eq!(
        app.post("/v1/platform/auth/refresh", NONE, json!({ "refresh_token": p0 }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.post("/v1/platform/auth/refresh", NONE, json!({ "refresh_token": p1 }))
            .await
            .status,
        StatusCode::UNAUTHORIZED
    );

    // Só o hash fica no banco.
    let db = w.env.db_name(w.tenant).await;
    let mut conn = w.env.migrator(&db).await;
    let lens: Vec<i32> = sqlx::query_scalar("SELECT length(token_hash) FROM refresh_tokens")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    assert!(lens.iter().all(|l| *l == 32));
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn profiles_catalog_and_audit() {
    let w = World::new().await;
    let app = &w.app;
    let m = app.get("/v1/modules", w.admin()).await;
    assert_eq!(m.status, StatusCode::OK);
    let codes: Vec<_> = m
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(codes, ["access", "users"]);
    assert_eq!(m.body[1]["features"].as_array().unwrap().len(), 4);

    let p = app.create_profile(w.admin(), "Leitores", &["users.read"]).await;
    let r = app.post("/v1/profiles", w.admin(), json!({ "name": "Leitores" })).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let r = app
        .post(
            "/v1/profiles",
            w.admin(),
            json!({ "name": "x", "features": ["nao.existe"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = app
        .patch(
            &format!("/v1/profiles/{p}"),
            w.admin(),
            json!({ "description": "só leitura" }),
        )
        .await;
    assert_eq!(r.body["description"], "só leitura");
    assert_eq!(
        app.get("/v1/profiles", w.admin()).await.body["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        app.delete(&format!("/v1/profiles/{p}"), w.admin()).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        app.get(&format!("/v1/profiles/{p}"), w.admin()).await.status,
        StatusCode::NOT_FOUND
    );

    // Ações de plataforma no tenant também ficam no access_audit.
    app.post(
        "/v1/users",
        tok_slug(&w.owner, SEED_SLUG),
        json!({ "email": "by@owner.io", "name": "x", "password": PASSWORD }),
    )
    .await;
    let db = w.env.db_name(w.tenant).await;
    let mut conn = w.env.migrator(&db).await;
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT action, actor_type FROM access_audit ORDER BY id")
        .fetch_all(&mut conn)
        .await
        .unwrap();
    for (a, t) in [
        ("tenant_admin.create", "platform"),
        ("profile.create", "tenant"),
        ("profile.update", "tenant"),
        ("profile.delete", "tenant"),
        ("user.create", "platform"),
    ] {
        assert!(rows.iter().any(|(x, y)| x == a && y == t), "{a}/{t} ausente: {rows:?}");
    }
    w.cleanup().await;
}

trait CreateStatus {
    async fn create_user_status(&self, token: &str, email: &str) -> StatusCode;
}

impl CreateStatus for TestApp {
    async fn create_user_status(&self, token: &str, email: &str) -> StatusCode {
        self.post(
            "/v1/users",
            tok(token),
            json!({ "email": email, "name": "n", "password": PASSWORD }),
        )
        .await
        .status
    }
}

fn source<'a>(body: &'a serde_json::Value, feature: &str) -> Option<&'a serde_json::Value> {
    body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["feature"] == feature)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn access_lists_sources_per_feature() {
    let w = World::new().await;
    let app = &w.app;
    let p = app
        .create_profile(w.admin(), "readers", &["users.read", "users.create"])
        .await;
    let uid = app.create_user(w.admin(), "u@x.io").await;
    assert_eq!(app.set_profiles(w.admin(), uid, &[p]).await.status, StatusCode::OK);
    for (code, effect) in [("access.read", "allow"), ("users.create", "deny")] {
        let r = app
            .put(
                &format!("/v1/users/{uid}/overrides/{code}"),
                w.admin(),
                json!({ "effect": effect, "reason": "teste" }),
            )
            .await;
        assert_eq!(r.status, StatusCode::OK, "{r:?}");
    }
    let r = app.get(&format!("/v1/users/{uid}/access"), w.admin()).await;
    assert_eq!(r.status, StatusCode::OK);
    let codes: Vec<_> = r.body["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["feature"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["access.read", "users.create", "users.read"]);
    assert_eq!(
        source(&r.body, "users.read").unwrap(),
        &json!({ "feature": "users.read", "effective": true,
                 "granted_by": [{ "kind": "profile", "profile_id": p }], "denied": false })
    );
    assert_eq!(
        source(&r.body, "users.create").unwrap(),
        &json!({ "feature": "users.create", "effective": false,
                 "granted_by": [{ "kind": "profile", "profile_id": p }], "denied": true })
    );
    assert_eq!(
        source(&r.body, "access.read").unwrap(),
        &json!({ "feature": "access.read", "effective": true,
                 "granted_by": [{ "kind": "allow" }], "denied": false })
    );
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn admin_sources_are_admin() {
    let w = World::new().await;
    let app = &w.app;
    let r = app.get(&format!("/v1/users/{}/access", w.admin_id), w.admin()).await;
    assert_eq!(r.status, StatusCode::OK);
    let sources = r.body["sources"].as_array().unwrap();
    let catalog: usize = app
        .get("/v1/modules", w.admin())
        .await
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["features"].as_array().unwrap().len())
        .sum();
    assert_eq!(sources.len(), catalog);
    for s in sources {
        assert_eq!(s["effective"], true, "{s}");
        assert!(
            s["granted_by"]
                .as_array()
                .unwrap()
                .contains(&json!({ "kind": "admin" })),
            "{s}"
        );
    }
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sources_ignore_expired_overrides() {
    let w = World::new().await;
    let app = &w.app;
    let uid = app.create_user(w.admin(), "u@x.io").await;
    let db = w.env.db_name(w.tenant).await;
    let mut conn = w.env.migrator(&db).await;
    for effect in ["allow", "deny"] {
        sqlx::query(
            "INSERT INTO user_feature_overrides (user_id, feature_id, effect, reason, expires_at, created_by, created_by_type)
             SELECT $1, id, $2, 'old', now() - interval '1 minute', $1, 'tenant' FROM features
             WHERE code = CASE $2 WHEN 'allow' THEN 'users.delete' ELSE 'users.update' END",
        )
        .bind(uid)
        .bind(effect)
        .execute(&mut conn)
        .await
        .unwrap();
    }
    let r = app.get(&format!("/v1/users/{uid}/access"), w.admin()).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.body["sources"].as_array().unwrap().is_empty(),
        "{:?}",
        r.body["sources"]
    );
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inactive_user_has_no_effective_features() {
    let w = World::new().await;
    let app = &w.app;
    let (uid, _) = app.user_with(w.admin(), SEED_SLUG, "u@x.io", &["users.read"]).await;
    let r = app
        .patch(&format!("/v1/users/{uid}"), w.admin(), json!({ "is_active": false }))
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r = app.get(&format!("/v1/users/{uid}/access"), w.admin()).await;
    assert_eq!(r.body["effective_features"], json!([]));
    let s = source(&r.body, "users.read").unwrap();
    assert_eq!(s["effective"], false);
    assert_eq!(s["granted_by"].as_array().unwrap().len(), 1);
    w.cleanup().await;
}
