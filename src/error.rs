//! Erros da API no formato RFC 9457 (`application/problem+json`).

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden(&'static str),
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(&'static str),
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

#[derive(Debug, Serialize, Clone)]
pub struct FieldError {
    pub field: &'static str,
    pub message: &'static str,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn invalid(field: &'static str, message: &'static str) -> Self {
        Self::Validation(vec![FieldError { field, message }])
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
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    errors: Option<&'a [FieldError]>,
}

pub fn problem(status: StatusCode, detail: Option<&str>, errors: Option<&[FieldError]>) -> Response {
    let body = Problem {
        kind: "about:blank",
        title: status.canonical_reason().unwrap_or("Error"),
        status: status.as_u16(),
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
            Self::Forbidden(d) | Self::Conflict(d) | Self::Unavailable(d) => problem(status, Some(d), None),
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
                Some("23505") => Self::Conflict("resource already exists"),
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
        Self::invalid("query", "invalid query parameters")
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
