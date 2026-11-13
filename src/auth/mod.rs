pub mod cookie;
pub mod handlers;
pub mod jwt;
pub mod middleware;
pub mod ratelimit;
pub mod routes;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use axum::extract::ConnectInfo;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ipnet::IpNet;
use sha2::{Digest, Sha256};

use crate::state::AppState;

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

/// IP do cliente: o par TCP ou, se o par for um proxy confiável, o IP de `X-Forwarded-For`.
pub struct ClientIp(pub IpAddr);

impl axum::extract::FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED), |c| c.0.ip());
        let forwarded = parts
            .headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect::<Vec<_>>()
            .join(",");
        Ok(ClientIp(client_ip(peer, Some(&forwarded), &state.cfg.trusted_proxies)))
    }
}

/// Se `peer` não é proxy confiável, devolve `peer`. Senão percorre `X-Forwarded-For` da direita
/// para a esquerda e devolve o primeiro IP não confiável; entrada que não é IP devolve `peer`;
/// se todos forem confiáveis, o mais à esquerda.
pub fn client_ip(peer: IpAddr, forwarded_for: Option<&str>, trusted: &[IpNet]) -> IpAddr {
    let is_trusted = |ip: &IpAddr| trusted.iter().any(|n| n.contains(ip));
    if !is_trusted(&peer) {
        return peer;
    }
    let Some(header) = forwarded_for else {
        return peer;
    };
    let mut leftmost = peer;
    for part in header.rsplit(',') {
        let Ok(ip) = part.trim().parse::<IpAddr>() else {
            return peer;
        };
        if !is_trusted(&ip) {
            return ip;
        }
        leftmost = ip;
    }
    leftmost
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn trusted() -> Vec<IpNet> {
        vec!["10.0.0.0/8".parse().unwrap()]
    }

    #[test]
    fn client_ip_rules() {
        let t = trusted();
        let cases = [
            ("203.0.113.9", Some("1.2.3.4"), "203.0.113.9"),
            ("10.0.0.2", Some("1.2.3.4"), "1.2.3.4"),
            ("10.0.0.2", Some("1.2.3.4, 10.0.0.5"), "1.2.3.4"),
            ("10.0.0.2", Some(" 1.2.3.4 ,10.0.0.5 "), "1.2.3.4"),
            ("10.0.0.2", Some("lixo"), "10.0.0.2"),
            ("10.0.0.2", None, "10.0.0.2"),
            ("10.0.0.2", Some("2001:db8::1"), "2001:db8::1"),
            ("10.0.0.2", Some("10.0.0.7, 10.0.0.5"), "10.0.0.7"),
            ("10.0.0.2", Some("9.9.9.9, lixo"), "10.0.0.2"),
        ];
        for (peer, xff, want) in cases {
            assert_eq!(client_ip(ip(peer), xff, &t), ip(want), "peer={peer} xff={xff:?}");
        }
    }
}
