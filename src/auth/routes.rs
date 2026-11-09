use axum::http::Method;
use axum::routing::{get, post};

use super::handlers as h;
use super::middleware::{Access, RouteDef, route};

pub fn routes() -> Vec<RouteDef> {
    vec![
        route(Method::POST, "/v1/auth/login", Access::Public, post(h::tenant_login)),
        route(
            Method::POST,
            "/v1/auth/refresh",
            Access::Public,
            post(h::tenant_refresh),
        ),
        route(
            Method::POST,
            "/v1/auth/logout",
            Access::TenantToken,
            post(h::tenant_logout),
        ),
        route(Method::GET, "/v1/me", Access::TenantToken, get(h::tenant_me)),
    ]
}
