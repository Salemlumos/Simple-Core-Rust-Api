use axum::http::Method;
use axum::routing::{delete, get, patch, post, put};

use super::catalog::Feature;
use super::handlers as h;
use crate::auth::middleware::{Access, RouteDef, route};

pub fn routes() -> Vec<RouteDef> {
    use Feature::*;
    vec![
        route(Method::GET, "/v1/modules", Access::Feature(AccessRead), get(h::modules)),
        route(
            Method::GET,
            "/v1/profiles",
            Access::Feature(AccessRead),
            get(h::list_profiles),
        ),
        route(
            Method::GET,
            "/v1/profiles/{id}",
            Access::Feature(AccessRead),
            get(h::get_profile),
        ),
        route(
            Method::POST,
            "/v1/profiles",
            Access::Feature(AccessProfilesManage),
            post(h::create_profile),
        ),
        route(
            Method::PATCH,
            "/v1/profiles/{id}",
            Access::Feature(AccessProfilesManage),
            patch(h::patch_profile),
        ),
        route(
            Method::DELETE,
            "/v1/profiles/{id}",
            Access::Feature(AccessProfilesManage),
            delete(h::delete_profile),
        ),
        route(
            Method::PUT,
            "/v1/profiles/{id}/features",
            Access::Feature(AccessProfilesManage),
            put(h::put_profile_features),
        ),
        route(
            Method::GET,
            "/v1/users/{id}/access",
            Access::Feature(AccessRead),
            get(h::user_access),
        ),
        route(
            Method::PUT,
            "/v1/users/{id}/profiles",
            Access::Feature(AccessUsersManage),
            put(h::put_user_profiles),
        ),
        route(
            Method::PUT,
            "/v1/users/{id}/overrides/{feature_code}",
            Access::Feature(AccessUsersManage),
            put(h::put_override),
        ),
        route(
            Method::DELETE,
            "/v1/users/{id}/overrides/{feature_code}",
            Access::Feature(AccessUsersManage),
            delete(h::delete_override),
        ),
    ]
}
