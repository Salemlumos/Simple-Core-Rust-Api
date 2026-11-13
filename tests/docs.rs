//! Swagger UI e especificação OpenAPI.

mod common;

use axum::http::StatusCode;
use common::*;

/// `api openapi` imprime a especificação sem banco nem variáveis de ambiente,
/// idêntica à servida em `/openapi.json` (usada para gerar o cliente do front).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_prints_the_same_spec_that_is_served() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_api"))
        .arg("openapi")
        .env_clear()
        .output()
        .expect("executa api openapi");
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let printed: serde_json::Value = serde_json::from_slice(&out.stdout).expect("stdout é só o JSON");

    let w = World::new().await;
    let mut cfg = w.env.api_config();
    cfg.api_docs = true;
    let app = w.env.app_with(cfg).await;
    let served = app.get("/openapi.json", NONE).await;
    assert_eq!(served.status, StatusCode::OK);
    assert_eq!(printed, served.body);
    drop(app);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn docs_are_served_only_when_enabled() {
    let w = World::new().await;
    // Padrão: desligado.
    assert_eq!(w.app.get("/openapi.json", NONE).await.status, StatusCode::NOT_FOUND);
    assert_eq!(w.app.get("/docs/", NONE).await.status, StatusCode::NOT_FOUND);

    let mut cfg = w.env.api_config();
    cfg.api_docs = true;
    let app = w.env.app_with(cfg).await;
    let spec = app.get("/openapi.json", NONE).await;
    assert_eq!(spec.status, StatusCode::OK);
    assert_eq!(spec.body["openapi"], "3.1.0");
    assert!(spec.body["paths"]["/v1/users/{id}"]["patch"].is_object());
    assert!(spec.body["components"]["securitySchemes"]["tenant_token"].is_object());
    let ui = app.get("/docs/", NONE).await;
    assert_eq!(ui.status, StatusCode::OK);
    assert!(
        ui.content_type.as_deref().unwrap_or("").starts_with("text/html"),
        "{:?}",
        ui.content_type
    );
    // A documentação não abre as rotas da API.
    assert_eq!(app.get("/v1/users", NONE).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        app.get("/v1/platform/tenants", NONE).await.status,
        StatusCode::UNAUTHORIZED
    );
    drop(app);
    w.cleanup().await;
}

/// No modo cookie o front não envia corpo em refresh/logout: a especificação
/// não pode marcá-lo como obrigatório.
#[test]
fn refresh_and_logout_bodies_are_optional() {
    let spec: serde_json::Value = serde_json::from_str(&simple_core::openapi::spec_json().unwrap()).unwrap();
    for path in [
        "/v1/auth/refresh",
        "/v1/auth/logout",
        "/v1/platform/auth/refresh",
        "/v1/platform/auth/logout",
    ] {
        let body = &spec["paths"][path]["post"]["requestBody"];
        assert!(body.is_object(), "{path} sem requestBody");
        assert_ne!(
            body["required"],
            serde_json::Value::Bool(true),
            "{path}: corpo obrigatório"
        );
    }
}
