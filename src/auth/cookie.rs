//! Refresh token em cookie `HttpOnly` para clientes de navegador.
//!
//! O cliente pede o modo com `X-Auth-Mode: cookie`. Nesse modo o login e o
//! refresh gravam o refresh token em um cookie `HttpOnly; SameSite=Strict`
//! restrito ao caminho de autenticação e o omitem do corpo; refresh e logout
//! leem o token do cookie. Sem o header, o token continua no corpo JSON.
//!
//! O header obrigatório também protege contra CSRF: um formulário de outro
//! site não consegue enviá-lo, e a API não libera CORS.

use axum::extract::{FromRequest, Request};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::handlers::{RefreshReq, TokenResp};
use crate::error::{ApiJson, AppError};
use crate::state::AppState;

/// Header com que o cliente pede o modo cookie (valor `cookie`).
pub const AUTH_MODE_HEADER: &str = "x-auth-mode";

/// Escopo da sessão: define nome e caminho do cookie.
#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Platform,
    Tenant,
}

impl Kind {
    pub const fn name(self) -> &'static str {
        match self {
            Kind::Platform => "sc_platform_rt",
            Kind::Tenant => "sc_tenant_rt",
        }
    }

    pub const fn path(self) -> &'static str {
        match self {
            Kind::Platform => "/v1/platform/auth",
            Kind::Tenant => "/v1/auth",
        }
    }
}

/// `true` quando a requisição pede o modo cookie.
pub fn cookie_mode(headers: &HeaderMap) -> bool {
    headers
        .get(AUTH_MODE_HEADER)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"cookie"))
}

/// Valor do cookie `kind` nos headers `Cookie` (vazio conta como ausente).
pub fn read(headers: &HeaderMap, kind: Kind) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == kind.name())
        .map(|(_, value)| value.trim().to_owned())
        .filter(|v| !v.is_empty())
}

fn build(kind: Kind, value: &str, max_age: u64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let s = format!(
        "{}={value}; Path={}; Max-Age={max_age}; HttpOnly; SameSite=Strict{secure}",
        kind.name(),
        kind.path()
    );
    // Nome, caminho e token (base64url) são ASCII visível.
    HeaderValue::from_str(&s).expect("cookie ASCII")
}

/// Cookie com o refresh token.
pub fn set(st: &AppState, kind: Kind, token: &str) -> HeaderValue {
    build(kind, token, st.cfg.refresh_ttl.as_secs(), st.cfg.auth_cookie_secure)
}

/// Cookie que apaga o refresh token no navegador.
pub fn clear(st: &AppState, kind: Kind) -> HeaderValue {
    build(kind, "", 0, st.cfg.auth_cookie_secure)
}

/// Refresh token recebido no corpo (padrão) ou no cookie (`X-Auth-Mode: cookie`).
pub enum RefreshInput {
    Body(String),
    Cookie(HeaderMap),
}

impl RefreshInput {
    pub fn is_cookie(&self) -> bool {
        matches!(self, RefreshInput::Cookie(_))
    }

    /// Token enviado pelo cliente; `None` no modo cookie sem cookie.
    pub fn token(&self, kind: Kind) -> Option<String> {
        match self {
            RefreshInput::Body(t) => Some(t.clone()),
            RefreshInput::Cookie(h) => read(h, kind),
        }
    }
}

impl FromRequest<AppState> for RefreshInput {
    type Rejection = AppError;

    async fn from_request(req: Request, st: &AppState) -> Result<Self, AppError> {
        if cookie_mode(req.headers()) {
            return Ok(RefreshInput::Cookie(req.headers().clone()));
        }
        let ApiJson(b) = ApiJson::<RefreshReq>::from_request(req, st).await?;
        Ok(RefreshInput::Body(b.refresh_token))
    }
}

/// Resposta com o novo par de tokens: no modo cookie, o refresh token vai só
/// no `Set-Cookie`.
pub fn tokens(st: &AppState, kind: Kind, cookie: bool, access: String, refresh: String) -> Response {
    let body = TokenResp {
        access_token: access,
        token_type: "Bearer",
        expires_in: st.jwt.ttl_secs(),
        refresh_token: (!cookie).then(|| refresh.clone()),
    };
    let mut resp = axum::Json(body).into_response();
    if cookie {
        resp.headers_mut().append(header::SET_COOKIE, set(st, kind, &refresh));
    }
    resp
}

/// Erro de refresh; no modo cookie, um 401 também apaga o cookie inválido.
pub fn refresh_error(st: &AppState, kind: Kind, cookie: bool, e: AppError) -> Response {
    let mut resp = e.into_response();
    if cookie && resp.status() == StatusCode::UNAUTHORIZED {
        resp.headers_mut().append(header::SET_COOKIE, clear(st, kind));
    }
    resp
}

/// 204 do logout; no modo cookie, apaga o cookie.
pub fn logged_out(st: &AppState, kind: Kind, cookie: bool) -> Response {
    let mut resp = StatusCode::NO_CONTENT.into_response();
    if cookie {
        resp.headers_mut().append(header::SET_COOKIE, clear(st, kind));
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(cookie: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, HeaderValue::from_str(cookie).unwrap());
        h
    }

    #[test]
    fn reads_the_right_cookie() {
        let h = headers("a=1; sc_tenant_rt=tok-1 ; sc_platform_rt=tok-2");
        assert_eq!(read(&h, Kind::Tenant).as_deref(), Some("tok-1"));
        assert_eq!(read(&h, Kind::Platform).as_deref(), Some("tok-2"));
    }

    #[test]
    fn empty_or_missing_cookie_is_none() {
        assert_eq!(read(&headers("sc_tenant_rt="), Kind::Tenant), None);
        assert_eq!(read(&headers("other=1"), Kind::Tenant), None);
        assert_eq!(read(&HeaderMap::new(), Kind::Tenant), None);
    }

    #[test]
    fn prefix_of_another_name_does_not_match() {
        assert_eq!(read(&headers("xsc_tenant_rt=a"), Kind::Tenant), None);
    }
}
