//! Erros da API no formato RFC 9457 (`application/problem+json`).
//!
//! Todo Problem traz um `code` estável em `snake_case`, e cada item de
//! `errors` também. Clientes devem decidir e traduzir mensagens pelo `code`;
//! `detail` e `message` são texto em inglês para quem lê a resposta e podem
//! mudar.

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Motivo de um 403 ou 409: código estável e explicação.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reason {
    pub code: &'static str,
    pub detail: &'static str,
}

/// Motivos de 403 e 409. Os códigos fazem parte do contrato da API.
pub mod reason {
    use super::Reason;

    const fn r(code: &'static str, detail: &'static str) -> Reason {
        Reason { code, detail }
    }

    // 403
    pub const MISSING_FEATURE: Reason = r("missing_feature", "missing feature");
    pub const MISSING_USERS_UPDATE: Reason = r("missing_feature", "missing feature users.update");
    pub const CANNOT_GRANT: Reason = r("cannot_grant", "cannot grant features you do not have");
    pub const OWN_ACCESS: Reason = r("cannot_change_own_access", "cannot change your own access");
    pub const ADMIN_CHANGE_PLATFORM_ONLY: Reason = r(
        "tenant_admin_platform_only",
        "only platform users can change a tenant admin",
    );
    pub const ADMIN_DELETE_PLATFORM_ONLY: Reason = r(
        "tenant_admin_platform_only",
        "only platform users can delete a tenant admin",
    );
    pub const INVALID_CURRENT_PASSWORD: Reason = r("invalid_current_password", "current password is invalid");
    pub const OWNER_ONLY: Reason = r("owner_only", "owner only");
    pub const PLATFORM_TOKEN_REQUIRED: Reason =
        r("platform_token_required", "tenant tokens cannot access platform routes");
    pub const TENANT_TOKEN_REQUIRED: Reason = r("tenant_token_required", "tenant token required");
    pub const TENANT_SUSPENDED: Reason = r("tenant_suspended", "tenant suspended");
    pub const TENANT_UNAVAILABLE: Reason = r("tenant_unavailable", "tenant not available");
    pub const TENANT_SLUG_MISMATCH: Reason = r("tenant_slug_mismatch", "X-Tenant-Slug does not match token");
    pub const TENANT_SLUG_REQUIRED: Reason = r("tenant_slug_required", "X-Tenant-Slug required for platform tokens");

    // 409
    pub const ALREADY_EXISTS: Reason = r("already_exists", "resource already exists");
    pub const EMAIL_TAKEN: Reason = r("email_taken", "email already in use");
    pub const SLUG_TAKEN: Reason = r("slug_taken", "slug already in use");
    pub const PROFILE_NAME_TAKEN: Reason = r("profile_name_taken", "profile name already in use");
    pub const TENANT_STATUS_TRANSITION: Reason = r(
        "invalid_status_transition",
        "tenant status can only change between active and suspended",
    );
    pub const TENANT_NOT_FAILED: Reason = r("tenant_not_failed", "only failed tenants can be retried");
    pub const TENANT_NOT_ACTIVE: Reason = r("tenant_not_active", "tenant is not active");
    pub const LAST_ADMIN_DEACTIVATE: Reason = r("last_tenant_admin", "cannot deactivate the last active tenant admin");
    pub const LAST_ADMIN_DELETE: Reason = r("last_tenant_admin", "cannot delete the last active tenant admin");
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {}", .0.code)]
    Forbidden(Reason),
    #[error("not found")]
    NotFound,
    #[error("conflict: {}", .0.code)]
    Conflict(Reason),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("unsupported media type")]
    UnsupportedMediaType,
    #[error("validation failed")]
    Validation(Vec<FieldError>),
    #[error("too many requests")]
    TooManyRequests,
    #[error("service unavailable: {0}")]
    Unavailable(&'static str),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Serialize, Clone, utoipa::ToSchema)]
pub struct FieldError {
    #[schema(value_type = String, example = "email")]
    pub field: &'static str,
    /// Código estável: `required`, `invalid_email`, `invalid_length`, `too_long`,
    /// `invalid_format`, `invalid_choice`, `out_of_range`, `too_many`, `unknown`,
    /// `must_be_future`, `not_allowed`, `no_changes`, `invalid_body`, `invalid_query`.
    #[schema(value_type = String, example = "invalid_email")]
    pub code: &'static str,
    #[schema(value_type = String)]
    pub message: &'static str,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn invalid(field: &'static str, code: &'static str, message: &'static str) -> Self {
        Self::Validation(vec![FieldError { field, code, message }])
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::Validation(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            Self::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    title: &'a str,
    status: u16,
    code: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<&'a [FieldError]>,
}

/// Código padrão de cada status (usado quando não há um [`Reason`]).
pub fn status_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "bad_request",
        StatusCode::UNAUTHORIZED => "unauthorized",
        StatusCode::FORBIDDEN => "forbidden",
        StatusCode::NOT_FOUND => "not_found",
        StatusCode::METHOD_NOT_ALLOWED => "method_not_allowed",
        StatusCode::CONFLICT => "conflict",
        StatusCode::PAYLOAD_TOO_LARGE => "payload_too_large",
        StatusCode::UNSUPPORTED_MEDIA_TYPE => "unsupported_media_type",
        StatusCode::UNPROCESSABLE_ENTITY => "validation_failed",
        StatusCode::TOO_MANY_REQUESTS => "rate_limited",
        StatusCode::SERVICE_UNAVAILABLE => "unavailable",
        _ => "internal",
    }
}

/// Problem com o código padrão do status.
pub fn problem(status: StatusCode, detail: Option<&str>, errors: Option<&[FieldError]>) -> Response {
    problem_with_code(status, status_code(status), detail, errors)
}

pub fn problem_with_code(
    status: StatusCode,
    code: &str,
    detail: Option<&str>,
    errors: Option<&[FieldError]>,
) -> Response {
    let body = Problem {
        kind: "about:blank",
        title: status.canonical_reason().unwrap_or("Error"),
        status: status.as_u16(),
        code,
        detail,
        errors,
    };
    let mut resp = (status, axum::Json(body)).into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    resp
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        match &self {
            Self::Internal(e) => {
                tracing::error!(error = ?e, "erro interno");
                metrics::counter!("app_internal_errors_total").increment(1);
                problem(status, None, None)
            }
            Self::Validation(errs) => problem(status, None, Some(errs)),
            Self::BadRequest(d) => problem(status, Some(d), None),
            Self::Forbidden(r) | Self::Conflict(r) => problem_with_code(status, r.code, Some(r.detail), None),
            Self::Unavailable(d) => problem(status, Some(d), None),
            _ => problem(status, None, None),
        }
    }
}

impl From<sqlx::Error> for AppError {
    fn from(e: sqlx::Error) -> Self {
        match &e {
            sqlx::Error::PoolTimedOut => {
                metrics::counter!("db_acquire_timeouts_total").increment(1);
                Self::Unavailable("database pool exhausted")
            }
            sqlx::Error::PoolClosed => Self::Unavailable("database pool closed"),
            sqlx::Error::RowNotFound => Self::NotFound,
            sqlx::Error::Database(d) => match d.code().as_deref() {
                Some("23505") => Self::Conflict(reason::ALREADY_EXISTS),
                Some("57014") => Self::Unavailable("statement timeout"),
                Some("40001" | "40P01") => Self::Unavailable("concurrent update, retry"),
                Some("53300") => Self::Unavailable("too many database connections"),
                _ => Self::Internal(e.into()),
            },
            _ => Self::Internal(e.into()),
        }
    }
}

/// Erros compartilhados pelo `try_get_with` do moka.
impl From<std::sync::Arc<sqlx::Error>> for AppError {
    fn from(e: std::sync::Arc<sqlx::Error>) -> Self {
        match &*e {
            sqlx::Error::PoolTimedOut => {
                metrics::counter!("db_acquire_timeouts_total").increment(1);
                Self::Unavailable("database pool exhausted")
            }
            sqlx::Error::PoolClosed => Self::Unavailable("database pool closed"),
            sqlx::Error::Database(d) if d.code().as_deref() == Some("57014") => Self::Unavailable("statement timeout"),
            _ => Self::Internal(anyhow::anyhow!("{e}")),
        }
    }
}

impl From<JsonRejection> for AppError {
    fn from(r: JsonRejection) -> Self {
        match r {
            JsonRejection::JsonDataError(e) => Self::BadRequest(e.body_text()).into_validation(),
            JsonRejection::JsonSyntaxError(_) => Self::BadRequest("malformed JSON body".into()),
            JsonRejection::MissingJsonContentType(_) => Self::UnsupportedMediaType,
            other if other.status() == StatusCode::PAYLOAD_TOO_LARGE => Self::PayloadTooLarge,
            other => Self::BadRequest(other.body_text()),
        }
    }
}

impl AppError {
    /// Corpo bem formado mas inválido (campo desconhecido, tipo errado): 422.
    fn into_validation(self) -> Self {
        match self {
            Self::BadRequest(detail) => {
                tracing::debug!(%detail, "json inválido");
                Self::Validation(vec![FieldError {
                    field: "body",
                    code: "invalid_body",
                    message: "invalid or unknown field",
                }])
            }
            other => other,
        }
    }
}

impl From<PathRejection> for AppError {
    fn from(_: PathRejection) -> Self {
        Self::NotFound
    }
}

impl From<QueryRejection> for AppError {
    fn from(_: QueryRejection) -> Self {
        Self::invalid("query", "invalid_query", "invalid query parameters")
    }
}

/// `Json` que rejeita no formato problem+json.
#[derive(axum::extract::FromRequest)]
#[from_request(via(axum::Json), rejection(AppError))]
pub struct ApiJson<T>(pub T);

#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct ApiPath<T>(pub T);

#[derive(axum::extract::FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct ApiQuery<T>(pub T);
