//! Refresh token em cookie `HttpOnly` (modo usado pelo front web).
//!
//! O cliente pede o modo com `X-Auth-Mode: cookie`. Sem o header, login,
//! refresh e logout continuam trocando o refresh token pelo corpo JSON.

mod common;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use common::*;
use serde_json::{Value, json};

const PLATFORM_COOKIE: &str = "sc_platform_rt";
const TENANT_COOKIE: &str = "sc_tenant_rt";

struct Call<'a> {
    path: &'a str,
    cookie_mode: bool,
    cookie: Option<(&'a str, &'a str)>,
    token: Option<&'a str>,
    slug: Option<&'a str>,
    body: Option<Value>,
}

impl<'a> Call<'a> {
    fn post(path: &'a str) -> Self {
        Call {
            path,
            cookie_mode: true,
            cookie: None,
            token: None,
            slug: None,
            body: None,
        }
    }

    async fn send(self, app: &TestApp) -> Resp {
        let mut req = Request::builder().method(Method::POST).uri(self.path);
        if self.cookie_mode {
            req = req.header("x-auth-mode", "cookie");
        }
        if let Some((name, value)) = self.cookie {
            req = req.header("cookie", format!("other=1; {name}={value}"));
        }
        if let Some(t) = self.token {
            req = req.header("authorization", format!("Bearer {t}"));
        }
        if let Some(s) = self.slug {
            req = req.header("x-tenant-slug", s);
        }
        let req = match self.body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        app.send(req).await
    }
}

/// Valor do cookie (`nome=valor; ...` → `valor`).
fn cookie_value(set_cookie: &str) -> &str {
    set_cookie.split(';').next().unwrap().split_once('=').unwrap().1
}

fn assert_cookie_attrs(set_cookie: &str, path: &str) {
    let attrs: Vec<String> = set_cookie
        .split(';')
        .skip(1)
        .map(|a| a.trim().to_ascii_lowercase())
        .collect();
    for want in [
        "httponly",
        "secure",
        "samesite=strict",
        &format!("path={}", path.to_ascii_lowercase()),
    ] {
        assert!(attrs.iter().any(|a| a == want), "falta {want} em {set_cookie}");
    }
    assert!(
        attrs.iter().any(|a| a.starts_with("max-age=")),
        "sem Max-Age: {set_cookie}"
    );
}

fn assert_cleared(set_cookie: &str) {
    assert_eq!(cookie_value(set_cookie), "", "{set_cookie}");
    assert!(set_cookie.to_ascii_lowercase().contains("max-age=0"), "{set_cookie}");
}

async fn platform_cookie_login(app: &TestApp) -> (String, String) {
    let mut c = Call::post("/v1/platform/auth/login");
    c.body = Some(json!({ "email": OWNER_EMAIL, "password": OWNER_PASSWORD }));
    let r = c.send(app).await;
    assert_eq!(r.status, StatusCode::OK, "{r:?}");
    let sc = r.set_cookie(PLATFORM_COOKIE).expect("Set-Cookie no login");
    (
        r.body["access_token"].as_str().unwrap().to_owned(),
        cookie_value(&sc).to_owned(),
    )
}

async fn platform_cookie_refresh(app: &TestApp, rt: &str) -> Resp {
    let mut c = Call::post("/v1/platform/auth/refresh");
    c.cookie = Some((PLATFORM_COOKIE, rt));
    c.send(app).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn platform_login_in_cookie_mode_sets_cookie_and_hides_refresh_token() {
    let w = World::new().await;
    let mut c = Call::post("/v1/platform/auth/login");
    c.body = Some(json!({ "email": OWNER_EMAIL, "password": OWNER_PASSWORD }));
    let r = c.send(&w.app).await;
    assert_eq!(r.status, StatusCode::OK, "{r:?}");
    assert!(r.body["access_token"].is_string());
    assert!(
        r.body.get("refresh_token").is_none(),
        "refresh_token vazou no corpo: {r:?}"
    );
    let sc = r.set_cookie(PLATFORM_COOKIE).expect("Set-Cookie");
    assert_cookie_attrs(&sc, "/v1/platform/auth");
    assert!(!cookie_value(&sc).is_empty());
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn body_mode_is_unchanged_and_sets_no_cookie() {
    let w = World::new().await;
    let r = w
        .app
        .post(
            "/v1/platform/auth/login",
            NONE,
            json!({ "email": OWNER_EMAIL, "password": OWNER_PASSWORD }),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body["refresh_token"].is_string());
    assert!(r.headers.get("set-cookie").is_none(), "{r:?}");
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn platform_refresh_via_cookie_rotates_and_detects_reuse() {
    let w = World::new().await;
    let (_, rt1) = platform_cookie_login(&w.app).await;

    let r = platform_cookie_refresh(&w.app, &rt1).await;
    assert_eq!(r.status, StatusCode::OK, "{r:?}");
    assert!(r.body["access_token"].is_string());
    assert!(r.body.get("refresh_token").is_none());
    let sc = r.set_cookie(PLATFORM_COOKIE).expect("cookie rotacionado");
    assert_cookie_attrs(&sc, "/v1/platform/auth");
    let rt2 = cookie_value(&sc).to_owned();
    assert_ne!(rt1, rt2);

    // Reusar o cookie antigo: 401, limpa o cookie e derruba a sessão inteira.
    let r = platform_cookie_refresh(&w.app, &rt1).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_cleared(&r.set_cookie(PLATFORM_COOKIE).expect("cookie limpo no 401"));
    let r = platform_cookie_refresh(&w.app, &rt2).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "sessão deveria ter sido revogada");
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cookie_refresh_requires_the_auth_mode_header() {
    let w = World::new().await;
    let (_, rt) = platform_cookie_login(&w.app).await;

    // Sem `X-Auth-Mode` (ex.: formulário de outro site), o cookie é ignorado.
    let mut c = Call::post("/v1/platform/auth/refresh");
    c.cookie_mode = false;
    c.cookie = Some((PLATFORM_COOKIE, &rt));
    let r = c.send(&w.app).await;
    assert_ne!(r.status, StatusCode::OK, "{r:?}");

    // O token não foi consumido.
    assert_eq!(platform_cookie_refresh(&w.app, &rt).await.status, StatusCode::OK);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cookie_refresh_without_cookie_is_unauthorized() {
    let w = World::new().await;
    let r = Call::post("/v1/platform/auth/refresh").send(&w.app).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{r:?}");
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn platform_logout_via_cookie_revokes_and_clears() {
    let w = World::new().await;
    let (access, rt) = platform_cookie_login(&w.app).await;

    let mut c = Call::post("/v1/platform/auth/logout");
    c.token = Some(&access);
    c.cookie = Some((PLATFORM_COOKIE, &rt));
    let r = c.send(&w.app).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{r:?}");
    assert_cleared(&r.set_cookie(PLATFORM_COOKIE).expect("cookie limpo"));

    assert_eq!(
        platform_cookie_refresh(&w.app, &rt).await.status,
        StatusCode::UNAUTHORIZED
    );
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_cookie_flow_login_refresh_logout() {
    let w = World::new().await;

    let mut c = Call::post("/v1/auth/login");
    c.slug = Some(SEED_SLUG);
    c.body = Some(json!({ "email": "admin@seed.io", "password": PASSWORD }));
    let r = c.send(&w.app).await;
    assert_eq!(r.status, StatusCode::OK, "{r:?}");
    assert!(r.body.get("refresh_token").is_none());
    let sc = r.set_cookie(TENANT_COOKIE).expect("Set-Cookie no login");
    assert_cookie_attrs(&sc, "/v1/auth");
    let rt1 = cookie_value(&sc).to_owned();

    let mut c = Call::post("/v1/auth/refresh");
    c.slug = Some(SEED_SLUG);
    c.cookie = Some((TENANT_COOKIE, &rt1));
    let r = c.send(&w.app).await;
    assert_eq!(r.status, StatusCode::OK, "{r:?}");
    let access = r.body["access_token"].as_str().unwrap().to_owned();
    let rt2 = cookie_value(&r.set_cookie(TENANT_COOKIE).expect("cookie rotacionado")).to_owned();
    assert_ne!(rt1, rt2);

    let mut c = Call::post("/v1/auth/logout");
    c.token = Some(&access);
    c.cookie = Some((TENANT_COOKIE, &rt2));
    let r = c.send(&w.app).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT, "{r:?}");
    assert_cleared(&r.set_cookie(TENANT_COOKIE).expect("cookie limpo"));

    let mut c = Call::post("/v1/auth/refresh");
    c.slug = Some(SEED_SLUG);
    c.cookie = Some((TENANT_COOKIE, &rt2));
    assert_eq!(c.send(&w.app).await.status, StatusCode::UNAUTHORIZED);
    w.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn secure_attribute_can_be_disabled() {
    let w = World::new().await;
    let mut cfg = w.env.api_config();
    cfg.auth_cookie_secure = false;
    let app = w.env.app_with(cfg).await;
    let mut c = Call::post("/v1/platform/auth/login");
    c.body = Some(json!({ "email": OWNER_EMAIL, "password": OWNER_PASSWORD }));
    let r = c.send(&app).await;
    let sc = r.set_cookie(PLATFORM_COOKIE).expect("Set-Cookie");
    assert!(!sc.to_ascii_lowercase().contains("secure"), "{sc}");
    assert!(sc.to_ascii_lowercase().contains("httponly"), "{sc}");
    drop(app);
    w.cleanup().await;
}
