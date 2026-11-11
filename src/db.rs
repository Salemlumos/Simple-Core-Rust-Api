//! Utilitários de banco: conexão com retentativa, identificadores e códigos de erro.

use std::time::{Duration, Instant};

use anyhow::bail;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{Connection, PgConnection, PgPool};

pub static CONTROL_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/control");
pub static TENANT_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/tenant");

/// Canal de invalidação de caches (banco control).
pub const CHANNEL_INVALIDATION: &str = "cache_invalidation";
/// Canal que acorda o worker de provisionamento (banco control).
pub const CHANNEL_PROVISIONING: &str = "tenant_provisioning";

/// Executa `f` até ter sucesso ou até estourar `DB_CONNECT_RETRY_SECONDS`.
pub async fn retry<T, F, Fut>(what: &str, limit: Duration, mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let start = Instant::now();
    let mut delay = Duration::from_millis(200);
    loop {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) if is_connect_error(&e) && start.elapsed() < limit => {
                tracing::warn!(target = what, error = %e, "banco indisponível, tentando novamente");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
            Err(e) => return Err(anyhow::Error::new(e).context(format!("falha ao conectar em {what}"))),
        }
    }
}

fn is_connect_error(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Io(_) | sqlx::Error::Tls(_) | sqlx::Error::PoolTimedOut => true,
        // 57P03 cannot_connect_now (startup), 53300 too_many_connections
        sqlx::Error::Database(d) => matches!(d.code().as_deref(), Some("57P03" | "53300")),
        _ => false,
    }
}

pub async fn connect(what: &str, opts: &PgConnectOptions, limit: Duration) -> anyhow::Result<PgConnection> {
    retry(what, limit, || PgConnection::connect_with(opts)).await
}

/// Pool que já testou a conectividade (com retentativa).
pub async fn pool(what: &str, opts: PgPoolOptions, conn: PgConnectOptions, limit: Duration) -> anyhow::Result<PgPool> {
    let pool = opts.connect_lazy_with(conn);
    retry(what, limit, || async { pool.acquire().await.map(drop) }).await?;
    Ok(pool)
}

/// Aspas de identificador SQL, escapando aspas duplas.
pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `^tenant_[0-9a-f]{32}$`
pub fn is_tenant_db_name(s: &str) -> bool {
    s.len() == 39
        && s.starts_with("tenant_")
        && s[7..].bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub fn tenant_db_name(id: uuid::Uuid) -> String {
    format!("tenant_{}", id.simple())
}

/// Nomes de role/banco vindos de configuração: só `[a-z_][a-z0-9_]*`.
pub fn validate_role_name(s: &str) -> anyhow::Result<()> {
    validate_db_name(s)
}

pub fn validate_db_name(s: &str) -> anyhow::Result<()> {
    let ok = !s.is_empty()
        && s.len() <= 63
        && s.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b == b'_')
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !ok {
        bail!("identificador inválido: {s:?}");
    }
    Ok(())
}

/// Slug de tenant: `^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$`
pub fn is_valid_slug(s: &str) -> bool {
    let b = s.as_bytes();
    let ok_char = |c: &u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    !b.is_empty()
        && b.len() <= 63
        && ok_char(&b[0])
        && ok_char(&b[b.len() - 1])
        && b.iter().all(|c| ok_char(c) || *c == b'-')
}

pub fn pg_code(e: &sqlx::Error) -> Option<String> {
    match e {
        sqlx::Error::Database(d) => d.code().map(|c| c.into_owned()),
        _ => None,
    }
}

pub fn is_unique_violation(e: &sqlx::Error) -> bool {
    pg_code(e).as_deref() == Some("23505")
}

pub fn is_fk_violation(e: &sqlx::Error) -> bool {
    pg_code(e).as_deref() == Some("23503")
}

/// Advisory lock por tenant, calculado no servidor para todos os processos usarem a mesma chave.
pub async fn lock_tenant(conn: &mut PgConnection, id: uuid::Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_lock(hashtextextended('app:tenant:' || $1::text, 0))")
        .bind(id)
        .execute(conn)
        .await
        .map(drop)
}

pub async fn unlock_tenant(conn: &mut PgConnection, id: uuid::Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended('app:tenant:' || $1::text, 0))")
        .bind(id)
        .execute(conn)
        .await
        .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_rules() {
        assert!(is_valid_slug("a"));
        assert!(is_valid_slug("acme-01"));
        assert!(!is_valid_slug("-acme"));
        assert!(!is_valid_slug("acme-"));
        assert!(!is_valid_slug("Acme"));
        assert!(!is_valid_slug(""));
        assert!(!is_valid_slug(&"a".repeat(64)));
        assert!(is_valid_slug(&"a".repeat(63)));
    }

    #[test]
    fn db_name_rules() {
        let id = uuid::Uuid::now_v7();
        assert!(is_tenant_db_name(&tenant_db_name(id)));
        assert!(!is_tenant_db_name("tenant_x"));
        assert!(!is_tenant_db_name("tenant_0123456789ABCDEF0123456789abcdef"));
        assert_eq!(quote_ident("a\"b"), "\"a\"\"b\"");
    }
}
