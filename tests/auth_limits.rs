//! Limite de tentativas de login: o 429 diz quando tentar de novo (`Retry-After`).

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
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

async fn login_from(app: &TestApp, peer: &str, xff: &str) -> Resp {
    let body = json!({ "email": "nobody@example.com", "password": "senha-errada" });
    let mut req = Request::builder()
        .method("POST")
        .uri("/v1/platform/auth/login")
        .header("content-type", "application/json")
        .header("x-forwarded-for", xff)
        .body(Body::from(body.to_string()))
        .unwrap();
    let addr = SocketAddr::new(peer.parse().unwrap(), 40000);
    req.extensions_mut().insert(ConnectInfo(addr));
    app.send(req).await
}

fn forwarded_cfg(w: &World) -> simple_core::config::ApiConfig {
    let mut cfg = w.env.api_config();
    cfg.login_window = Duration::from_secs(60);
    cfg.login_limit_platform = RateLimit {
        per_ip: 2,
        per_email: 1000,
    };
    cfg.trusted_proxies = vec!["10.0.0.0/8".parse().unwrap()];
    cfg
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forwarded_for_from_trusted_proxy_is_used() {
    let w = World::new().await;
    let app = w.env.app_with(forwarded_cfg(&w)).await;

    for xff in ["1.1.1.1", "2.2.2.2", "3.3.3.3"] {
        let r = login_from(&app, "10.0.0.2", xff).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{xff}: {r:?}");
    }
    for _ in 0..2 {
        let r = login_from(&app, "10.0.0.2", "4.4.4.4").await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{r:?}");
    }
    let r = login_from(&app, "10.0.0.2", "4.4.4.4").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "{r:?}");

    drop(app);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forged_forwarded_for_is_ignored() {
    let w = World::new().await;
    let app = w.env.app_with(forwarded_cfg(&w)).await;

    for (i, xff) in ["1.1.1.1", "2.2.2.2", "3.3.3.3"].into_iter().enumerate() {
        let r = login_from(&app, "203.0.113.9", xff).await;
        let want = if i < 2 {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::TOO_MANY_REQUESTS
        };
        assert_eq!(r.status, want, "{xff}: {r:?}");
    }

    drop(app);
    w.cleanup().await;
}
