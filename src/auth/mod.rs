pub mod handlers;
pub mod jwt;
pub mod middleware;
pub mod ratelimit;
pub mod routes;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::ConnectInfo;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Refresh token opaco (256 bits aleatórios) e o SHA-256 que vai para o banco.
pub fn new_refresh_token() -> (String, Vec<u8>) {
    let mut raw = [0u8; 32];
    rand::fill(&mut raw);
    let token = b64(&raw);
    let hash = sha256(&token);
    (token, hash)
}

pub fn sha256(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// IP do cliente (endereço do par TCP).
pub struct ClientIp(pub IpAddr);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut axum::http::request::Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientIp(
            parts
                .extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.ip()),
        ))
    }
}
