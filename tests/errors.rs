//! Códigos estáveis nos erros (`code` no Problem e em cada `errors[]`).
//!
//! O front traduz as mensagens a partir desses códigos; `detail` e `message`
//! continuam em inglês, só para quem lê a resposta.

mod common;

use axum::http::StatusCode;
use common::*;
use serde_json::json;

fn field_code<'a>(r: &'a Resp, field: &str) -> Option<&'a str> {
    r.body["errors"].as_array()?.iter().find(|e| e["field"] == field)?["code"].as_str()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_problem_has_a_code() {
    let w = World::new().await;
    let app = &w.app;

    let r = app.get("/v1/users", NONE).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(r.body["code"], "unauthorized", "{r:?}");

    let r = app.get("/v1/nope", NONE).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.body["code"], "not_found", "{r:?}");

    let r = app.get(&format!("/v1/users/{}", uuid::Uuid::now_v7()), w.admin()).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.body["code"], "not_found", "{r:?}");

    let r = app.get("/v1/users?limit=0", w.admin()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.body["code"], "validation_failed", "{r:?}");
    assert_eq!(field_code(&r, "limit"), Some("out_of_range"), "{r:?}");
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forbidden_and_conflict_carry_specific_codes() {
    let w = World::new().await;
    let app = &w.app;
    let (_, common) = app
        .user_with(
            w.admin(),
            SEED_SLUG,
            "common@seed.io",
            &["users.read", "users.create", "access.profiles.manage"],
        )
        .await;

    // Sem a feature.
    let r = app.delete(&format!("/v1/users/{}", w.admin_id), tok(&common)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["code"], "missing_feature", "{r:?}");
    assert!(r.body["detail"].is_string(), "detail continua presente: {r:?}");

    // Conceder feature que não tem.
    let r = app
        .post(
            "/v1/profiles",
            tok(&common),
            json!({ "name": "esc", "features": ["users.delete"] }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{r:?}");
    assert_eq!(r.body["code"], "cannot_grant", "{r:?}");

    // Token de tenant em rota de plataforma.
    let r = app.get("/v1/platform/tenants", tok(&common)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["code"], "platform_token_required", "{r:?}");

    // E-mail repetido.
    let r = app
        .post(
            "/v1/users",
            w.admin(),
            json!({ "email": "common@seed.io", "name": "x", "password": PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["code"], "email_taken", "{r:?}");

    // Slug repetido.
    let r = app
        .post(
            "/v1/platform/tenants",
            tok(&w.owner),
            json!({ "slug": SEED_SLUG, "name": "x" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["code"], "slug_taken", "{r:?}");

    // Senha atual errada.
    let r = app
        .patch(
            "/v1/platform/me",
            tok(&w.owner),
            json!({ "password": "another-password-123", "current_password": "wrong-password-123" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.body["code"], "invalid_current_password", "{r:?}");
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn field_errors_carry_codes() {
    let w = World::new().await;
    let app = &w.app;

    let r = app
        .post(
            "/v1/users",
            w.admin(),
            json!({ "email": "not-an-email", "name": "", "password": "short" }),
        )
        .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(field_code(&r, "email"), Some("invalid_email"), "{r:?}");
    assert_eq!(field_code(&r, "name"), Some("invalid_length"), "{r:?}");
    assert_eq!(field_code(&r, "password"), Some("invalid_length"), "{r:?}");
    for e in r.body["errors"].as_array().unwrap() {
        assert!(e["message"].is_string(), "message continua presente: {e}");
    }

    let r = app.post("/v1/users", w.admin(), json!({ "unknown": 1 })).await;
    assert_eq!(field_code(&r, "body"), Some("invalid_body"), "{r:?}");

    let r = app
        .post(
            "/v1/platform/tenants",
            tok(&w.owner),
            json!({ "slug": "Bad Slug", "name": "x" }),
        )
        .await;
    assert_eq!(field_code(&r, "slug"), Some("invalid_format"), "{r:?}");

    let r = app.patch("/v1/platform/me", tok(&w.owner), json!({})).await;
    assert_eq!(field_code(&r, "body"), Some("no_changes"), "{r:?}");

    let r = app
        .patch(
            "/v1/platform/me",
            tok(&w.owner),
            json!({ "password": "another-password-123" }),
        )
        .await;
    assert_eq!(field_code(&r, "current_password"), Some("required"), "{r:?}");
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spec_documents_the_codes() {
    let spec: serde_json::Value = serde_json::from_str(&simple_core::openapi::spec_json().unwrap()).unwrap();
    let schemas = &spec["components"]["schemas"];
    assert!(
        schemas["Problem"]["properties"]["code"].is_object(),
        "{}",
        schemas["Problem"]
    );
    assert!(
        schemas["FieldError"]["properties"]["code"].is_object(),
        "{}",
        schemas["FieldError"]
    );
}
