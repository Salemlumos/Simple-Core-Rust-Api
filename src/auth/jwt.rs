//! Access tokens JWT HS256 com algoritmo fixo, `exp`, `iss` e `aud` validados.
//! Plataforma e tenant usam segredos e audiências diferentes. Nenhum token
//! carrega papel ou permissões.

use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{ApiConfig, PLATFORM_AUDIENCE, TENANT_AUDIENCE};

#[derive(Debug, Serialize, Deserialize)]
pub struct TenantClaims {
    pub sub: Uuid,
    pub tid: Uuid,
    pub iat: i64,
    pub exp: i64,
    pub iss: String,
    pub aud: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlatformClaims {
    pub sub: Uuid,
    pub iat: i64,
    pub exp: i64,
    pub iss: String,
    pub aud: String,
}

pub struct JwtKeys {
    issuer: String,
    ttl_secs: i64,
    platform_enc: EncodingKey,
    platform_dec: DecodingKey,
    platform_val: Validation,
    tenant_enc: EncodingKey,
    tenant_dec: DecodingKey,
    tenant_val: Validation,
}

fn validation(issuer: &str, aud: &str) -> Validation {
    let mut v = Validation::new(Algorithm::HS256);
    v.algorithms = vec![Algorithm::HS256];
    v.set_issuer(&[issuer]);
    v.set_audience(&[aud]);
    v.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    v.validate_exp = true;
    v.leeway = 5;
    v
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

impl JwtKeys {
    pub fn new(cfg: &ApiConfig) -> Self {
        let p = cfg.jwt_platform_secret.expose().as_bytes();
        let t = cfg.jwt_tenant_secret.expose().as_bytes();
        Self {
            issuer: cfg.jwt_issuer.clone(),
            ttl_secs: cfg.access_ttl.as_secs() as i64,
            platform_enc: EncodingKey::from_secret(p),
            platform_dec: DecodingKey::from_secret(p),
            platform_val: validation(&cfg.jwt_issuer, PLATFORM_AUDIENCE),
            tenant_enc: EncodingKey::from_secret(t),
            tenant_dec: DecodingKey::from_secret(t),
            tenant_val: validation(&cfg.jwt_issuer, TENANT_AUDIENCE),
        }
    }

    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    pub fn issue_tenant(&self, user: Uuid, tenant: Uuid) -> anyhow::Result<String> {
        let iat = now();
        let c = TenantClaims {
            sub: user,
            tid: tenant,
            iat,
            exp: iat + self.ttl_secs,
            iss: self.issuer.clone(),
            aud: TENANT_AUDIENCE.into(),
        };
        Ok(encode(&Header::new(Algorithm::HS256), &c, &self.tenant_enc)?)
    }

    pub fn issue_platform(&self, user: Uuid) -> anyhow::Result<String> {
        let iat = now();
        let c = PlatformClaims {
            sub: user,
            iat,
            exp: iat + self.ttl_secs,
            iss: self.issuer.clone(),
            aud: PLATFORM_AUDIENCE.into(),
        };
        Ok(encode(&Header::new(Algorithm::HS256), &c, &self.platform_enc)?)
    }

    pub fn verify_tenant(&self, token: &str) -> Option<TenantClaims> {
        decode::<TenantClaims>(token, &self.tenant_dec, &self.tenant_val)
            .ok()
            .map(|d| d.claims)
    }

    pub fn verify_platform(&self, token: &str) -> Option<PlatformClaims> {
        decode::<PlatformClaims>(token, &self.platform_dec, &self.platform_val)
            .ok()
            .map(|d| d.claims)
    }
}
