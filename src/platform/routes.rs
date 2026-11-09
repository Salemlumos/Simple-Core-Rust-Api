use axum::http::Method;
use axum::routing::{delete, get, patch, post};

use super::handlers as h;
use crate::auth::handlers as auth;
use crate::auth::middleware::{Access, RouteDef, route};

pub fn routes() -> Vec<RouteDef> {
    use Access::{Platform, PlatformOwner, Public};
    vec![
        route(
            Method::POST,
            "/v1/platform/auth/login",
            Public,
            post(auth::platform_login),
        ),
        route(
            Method::POST,
            "/v1/platform/auth/refresh",
            Public,
            post(auth::platform_refresh),
        ),
        route(
            Method::POST,
            "/v1/platform/auth/logout",
            Platform,
            post(auth::platform_logout),
        ),
        route(Method::GET, "/v1/platform/me", Platform, get(h::me)),
        route(Method::PATCH, "/v1/platform/me", Platform, patch(h::patch_me)),
        route(Method::GET, "/v1/platform/tenants", Platform, get(h::list_tenants)),
        route(Method::GET, "/v1/platform/tenants/{id}", Platform, get(h::get_tenant)),
        route(
            Method::POST,
            "/v1/platform/tenants",
            PlatformOwner,
            post(h::create_tenant),
        ),
        route(
            Method::PATCH,
            "/v1/platform/tenants/{id}",
            PlatformOwner,
            patch(h::patch_tenant),
        ),
        route(
            Method::POST,
            "/v1/platform/tenants/{id}/retry",
            PlatformOwner,
            post(h::retry_tenant),
        ),
        route(
            Method::GET,
            "/v1/platform/superusers",
            PlatformOwner,
            get(h::list_superusers),
        ),
        route(
            Method::POST,
            "/v1/platform/superusers",
            PlatformOwner,
            post(h::create_superuser),
        ),
        route(
            Method::PATCH,
            "/v1/platform/superusers/{id}",
            PlatformOwner,
            patch(h::patch_superuser),
        ),
        route(
            Method::DELETE,
            "/v1/platform/superusers/{id}",
            PlatformOwner,
            delete(h::delete_superuser),
        ),
        route(
            Method::GET,
            "/v1/platform/tenants/{id}/admins",
            PlatformOwner,
            get(h::list_admins),
        ),
        route(
            Method::POST,
            "/v1/platform/tenants/{id}/admins",
            PlatformOwner,
            post(h::create_admin),
        ),
        route(
            Method::DELETE,
            "/v1/platform/tenants/{id}/admins/{user_id}",
            PlatformOwner,
            delete(h::delete_admin),
        ),
    ]
}
