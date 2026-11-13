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
    code: &'static str,
    detail: &'static str,
}

impl Reason {
    pub const fn code(self) -> &'static str {
        self.code
    }

    pub const fn detail(self) -> &'static str {
        self.detail
    }
}

/// Código de um erro de campo (422). Só se constrói aqui, pelas constantes de
/// [`field`], para que a lista publicada na especificação seja a lista inteira.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct FieldCode(&'static str);

impl FieldCode {
    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

/// Gera as constantes de [`field`] e `ALL` a partir de uma única lista.
macro_rules! field_codes {
    ($($name:ident = $code:literal),* $(,)?) => {
        /// Códigos de erro de campo. Fazem parte do contrato da API.
        pub mod field {
            use super::FieldCode;

            $(pub const $name: FieldCode = FieldCode($code);)*

            pub const ALL: &[FieldCode] = &[$($name),*];
        }
    };
}

field_codes! {
    REQUIRED = "required",
    INVALID_EMAIL = "invalid_email",
    INVALID_LENGTH = "invalid_length",
    TOO_LONG = "too_long",
    INVALID_FORMAT = "invalid_format",
    INVALID_CHOICE = "invalid_choice",
    OUT_OF_RANGE = "out_of_range",
    TOO_MANY = "too_many",
    UNKNOWN = "unknown",
    MUST_BE_FUTURE = "must_be_future",
    NOT_ALLOWED = "not_allowed",
    NO_CHANGES = "no_changes",
    INVALID_BODY = "invalid_body",
    INVALID_QUERY = "invalid_query",
}

/// Gera as constantes de [`reason`] e `ALL` a partir de uma única lista.
macro_rules! reasons {
    ($($name:ident = ($code:literal, $detail:literal)),* $(,)?) => {
        /// Motivos de 403 e 409. Os códigos fazem parte do contrato da API.
        pub mod reason {
            use super::Reason;

            $(pub const $name: Reason = Reason { code: $code, detail: $detail };)*

            pub const ALL: &[Reason] = &[$($name),*];
        }
    };
}

reasons! {
    // 403
    MISSING_FEATURE = ("missing_feature", "missing feature"),
    MISSING_USERS_UPDATE = ("missing_feature", "missing feature users.update"),
    CANNOT_GRANT = ("cannot_grant", "cannot grant features you do not have"),
    OWN_ACCESS = ("cannot_change_own_access", "cannot change your own access"),
    ADMIN_CHANGE_PLATFORM_ONLY = (
        "tenant_admin_platform_only",
        "only platform users can change a tenant admin"
    ),
    ADMIN_DELETE_PLATFORM_ONLY = (
        "tenant_admin_platform_only",
        "only platform users can delete a tenant admin"
    ),
    INVALID_CURRENT_PASSWORD = ("invalid_current_password", "current password is invalid"),
    OWNER_ONLY = ("owner_only", "owner only"),
    PLATFORM_TOKEN_REQUIRED = ("platform_token_required", "tenant tokens cannot access platform routes"),
    TENANT_TOKEN_REQUIRED = ("tenant_token_required", "tenant token required"),
    TENANT_SUSPENDED = ("tenant_suspended", "tenant suspended"),
    TENANT_UNAVAILABLE = ("tenant_unavailable", "tenant not available"),
    TENANT_SLUG_MISMATCH = ("tenant_slug_mismatch", "X-Tenant-Slug does not match token"),
    TENANT_SLUG_REQUIRED = ("tenant_slug_required", "X-Tenant-Slug required for platform tokens"),

    // 409
    ALREADY_EXISTS = ("already_exists", "resource already exists"),
    EMAIL_TAKEN = ("email_taken", "email already in use"),
    SLUG_TAKEN = ("slug_taken", "slug already in use"),
    PROFILE_NAME_TAKEN = ("profile_name_taken", "profile name already in use"),
    TENANT_STATUS_TRANSITION = (
        "invalid_status_transition",
        "tenant status can only change between active and suspended"
    ),
    TENANT_NOT_FAILED = ("tenant_not_failed", "only failed tenants can be retried"),
    TENANT_NOT_ACTIVE = ("tenant_not_active", "tenant is not active"),
    LAST_ADMIN_DEACTIVATE = ("last_tenant_admin", "cannot deactivate the last active tenant admin"),
    LAST_ADMIN_DELETE = ("last_tenant_admin", "cannot delete the last active tenant admin"),
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {}", .0.code())]
    Forbidden(Reason),
    #[error("not found")]
    NotFound,
    #[error("conflict: {}", .0.code())]
    Conflict(Reason),
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("unsupported media type")]
    UnsupportedMediaType,
    #[error("validation failed")]
    Validation(Vec<FieldError>),
    #[error("too many requests")]
    TooManyRequests { retry_after_secs: u64 },
    #[error("service unavailable: {0}")]
    Unavailable(&'static str),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

#[derive(Debug, Serialize, Clone, utoipa::ToSchema)]
pub struct FieldError {
    #[schema(value_type = String, example = "email")]
    pub field: &'static str,
    /// Código estável; a lista fechada está no `enum` do schema (`field::ALL`).
    #[schema(value_type = String, example = "invalid_email")]
    pub code: FieldCode,
    #[schema(value_type = String)]
    pub message: &'static str,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn invalid(field: &'static str, code: FieldCode, message: &'static str) -> Self {
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
            Self::TooManyRequests { .. } => StatusCode::TOO_MANY_REQUESTS,
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

/// Todos os valores que [`status_code`] pode devolver.
pub const STATUS_CODES: &[&str] = &[
    "bad_request",
    "unauthorized",
    "forbidden",
    "not_found",
    "method_not_allowed",
    "conflict",
    "payload_too_large",
    "unsupported_media_type",
    "validation_failed",
    "rate_limited",
    "unavailable",
    "internal",
];

/// Todo valor possível de `Problem.code`: padrões de status e motivos de 403/409, sem repetição e ordenado.
pub fn problem_codes() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = STATUS_CODES
        .iter()
        .copied()
        .chain(reason::ALL.iter().map(|r| r.code()))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
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

fn problem_with_code(
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
            Self::Forbidden(r) | Self::Conflict(r) => problem_with_code(status, r.code(), Some(r.detail()), None),
            Self::Unavailable(d) => problem(status, Some(d), None),
            Self::TooManyRequests { retry_after_secs } => {
                let mut resp = problem(status, None, None);
                resp.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(*retry_after_secs));
                resp
            }
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
                    code: field::INVALID_BODY,
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
        Self::invalid("query", field::INVALID_QUERY, "invalid query parameters")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn snake_case(s: &str) -> bool {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    #[test]
    fn status_code_values_are_listed() {
        for n in 100u16..=599 {
            if let Ok(s) = StatusCode::from_u16(n) {
                assert!(
                    STATUS_CODES.contains(&status_code(s)),
                    "{n} devolve código fora de STATUS_CODES"
                );
            }
        }
    }

    #[test]
    fn codes_are_unique_snake_case() {
        let mut seen = std::collections::BTreeSet::new();
        for c in field::ALL {
            assert!(snake_case(c.as_str()), "{}", c.as_str());
            assert!(seen.insert(c.as_str()), "repetido: {}", c.as_str());
        }
        for c in problem_codes() {
            assert!(snake_case(c), "{c}");
        }
    }
}
