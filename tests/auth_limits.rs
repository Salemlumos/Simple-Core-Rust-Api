//! Limite de tentativas de login: o 429 diz quando tentar de novo (`Retry-After`).

mod common;

use std::time::Duration;

use axum::http::StatusCode;
use common::*;
use serde_json::json;
use simple_core::config::RateLimit;

fn retry_after(r: &Resp) -> u64 {
    r.headers["retry-after"]
        .to_str()
        .expect("retry-after ascii")
        .parse()
        .expect("retry-after inteiro")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_login_has_retry_after() {
    let w = World::new().await;
    let mut cfg = w.env.api_config();
    cfg.login_window = Duration::from_secs(60);
    cfg.login_limit_platform = RateLimit {
        per_ip: 1000,
        per_email: 2,
    };
    let app = w.env.app_with(cfg).await;

    let body = json!({ "email": "nobody@example.com", "password": "senha-errada" });
    for _ in 0..2 {
        let r = app.post("/v1/platform/auth/login", NONE, body.clone()).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{r:?}");
        assert!(r.headers.get("retry-after").is_none());
    }
    let r = app.post("/v1/platform/auth/login", NONE, body).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{r:?}");
    assert_eq!(r.body["code"], "rate_limited");
    let secs = retry_after(&r);
    assert!((1..=60).contains(&secs), "retry-after fora da janela: {secs}");

    drop(app);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_is_within_window() {
    let w = World::new().await;
    let mut cfg = w.env.api_config();
    cfg.login_window = Duration::from_secs(2);
    cfg.login_limit_platform = RateLimit {
        per_ip: 1000,
        per_email: 1,
    };
    let app = w.env.app_with(cfg).await;

    let body = json!({ "email": "nobody@example.com", "password": "senha-errada" });
    let r = app.post("/v1/platform/auth/login", NONE, body.clone()).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{r:?}");
    let r = app.post("/v1/platform/auth/login", NONE, body).await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{r:?}");
    assert_eq!(r.body["code"], "rate_limited");
    let secs = retry_after(&r);
    assert!((1..=2).contains(&secs), "retry-after fora da janela: {secs}");

    drop(app);
    w.cleanup().await;
}
