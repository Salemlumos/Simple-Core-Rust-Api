//! Swagger UI e especificação OpenAPI.

mod common;

use axum::http::StatusCode;
use common::*;

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
