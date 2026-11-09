use axum::http::Method;
use axum::routing::{delete, get, patch, post};

use super::handlers as h;
use crate::access::catalog::Feature;
use crate::auth::middleware::{Access, RouteDef, route};

pub fn routes() -> Vec<RouteDef> {
    vec![
        route(
            Method::POST,
            "/v1/users",
            Access::Feature(Feature::UsersCreate),
            post(h::create),
        ),
        route(
            Method::GET,
            "/v1/users",
            Access::Feature(Feature::UsersRead),
            get(h::list),
        ),
        route(
            Method::GET,
            "/v1/users/{id}",
            Access::Feature(Feature::UsersRead),
            get(h::get),
        ),
        // O próprio usuário altera o nome sem feature; o handler restringe os campos.
        route(
            Method::PATCH,
            "/v1/users/{id}",
            Access::SelfOr(Feature::UsersUpdate),
            patch(h::patch),
        ),
        route(
            Method::DELETE,
            "/v1/users/{id}",
            Access::Feature(Feature::UsersDelete),
            delete(h::delete),
        ),
    ]
}
