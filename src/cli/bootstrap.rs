//! `api bootstrap`: cria de forma idempotente `app_migrator`, `app_runtime` e o
//! banco `control`, com as credenciais administrativas.
//!
//! - Nunca altera a senha de uma role existente.
//! - As senhas vão ao servidor como verificadores SCRAM-SHA-256 calculados
//!   aqui: o texto da senha não aparece em nenhum SQL nem em log.
//! - Com `DB_BOOTSTRAP=false`, apenas valida (com as credenciais do migrator)
//!   que as roles e o `control` existem com os atributos corretos.

use anyhow::{Context, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};
use sqlx::{AssertSqlSafe, Connection, PgConnection};

use crate::config::{DbConfig, Secret, env_bool};
use crate::db::{self, quote_ident};

const SCRAM_ITERATIONS: u32 = 4096;

/// `SCRAM-SHA-256$<iter>:<salt>$<StoredKey>:<ServerKey>` (RFC 5802/7677).
pub fn scram_verifier(password: &str) -> String {
    let mut salt = [0u8; 16];
    rand::fill(&mut salt);
    scram_verifier_with_salt(password, &salt, SCRAM_ITERATIONS)
}

fn scram_verifier_with_salt(password: &str, salt: &[u8], iterations: u32) -> String {
    let mut salted = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, &mut salted);
    let hmac = |key: &[u8], msg: &[u8]| {
        let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("hmac aceita qualquer chave");
        m.update(msg);
        m.finalize().into_bytes()
    };
    let client_key = hmac(&salted, b"Client Key");
    let stored_key = Sha256::digest(client_key);
    let server_key = hmac(&salted, b"Server Key");
    format!(
        "SCRAM-SHA-256${iterations}:{}${}:{}",
        STANDARD.encode(salt),
        STANDARD.encode(stored_key),
        STANDARD.encode(server_key)
    )
}

struct RoleAttrs {
    exists: bool,
    createdb: bool,
    superuser: bool,
}

async fn role(conn: &mut PgConnection, name: &str) -> anyhow::Result<RoleAttrs> {
    let row: Option<(bool, bool)> = sqlx::query_as("SELECT rolcreatedb, rolsuper FROM pg_roles WHERE rolname = $1")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(match row {
        Some((createdb, superuser)) => RoleAttrs {
            exists: true,
            createdb,
            superuser,
        },
        None => RoleAttrs {
            exists: false,
            createdb: false,
            superuser: false,
        },
    })
}

async fn database_exists(conn: &mut PgConnection, name: &str) -> anyhow::Result<bool> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1)")
            .bind(name)
            .fetch_one(&mut *conn)
            .await?,
    )
}

async fn exec(conn: &mut PgConnection, sql: String) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(AssertSqlSafe(sql)).execute(conn).await.map(drop)
}

async fn create_role(conn: &mut PgConnection, name: &str, password: &Secret, attrs: &str) -> anyhow::Result<()> {
    // O verificador só contém [A-Za-z0-9+/=:$-]; seguro como literal.
    let verifier = scram_verifier(password.expose());
    let sql = format!("CREATE ROLE {} LOGIN {attrs} PASSWORD '{verifier}'", quote_ident(name));
    match exec(conn, sql).await {
        Ok(()) => {
            tracing::info!(role = name, "role criada");
            Ok(())
        }
        // Corrida com outro bootstrap: a role já existe e não mexemos na senha.
        Err(e) if db::pg_code(&e).as_deref() == Some("42710") => Ok(()),
        Err(e) => Err(anyhow::Error::new(e).context(format!("falha ao criar a role {name}"))),
    }
}

fn check_roles(migrator: &RoleAttrs, runtime: &RoleAttrs, db: &DbConfig) -> anyhow::Result<()> {
    if !migrator.exists || !runtime.exists {
        bail!("roles {} e/ou {} não existem", db.migrator_user, db.runtime_user);
    }
    if !migrator.createdb {
        bail!("a role {} precisa de CREATEDB", db.migrator_user);
    }
    if runtime.createdb {
        bail!("a role {} não pode ter CREATEDB", db.runtime_user);
    }
    if migrator.superuser || runtime.superuser {
        bail!("{} e {} não podem ser superusuárias", db.migrator_user, db.runtime_user);
    }
    Ok(())
}

pub async fn run(db: &DbConfig) -> anyhow::Result<()> {
    if env_bool("DB_BOOTSTRAP", true)? {
        bootstrap(db).await
    } else {
        validate(db).await
    }
}

pub async fn bootstrap(db: &DbConfig) -> anyhow::Result<()> {
    let mut conn = db::connect("admin", &db.admin(&db.admin_db)?, db.connect_retry).await?;

    let migrator = role(&mut conn, &db.migrator_user).await?;
    if !migrator.exists {
        let pass = db
            .migrator_password
            .as_ref()
            .context("DB_MIGRATOR_PASSWORD obrigatório para criar a role")?;
        create_role(&mut conn, &db.migrator_user, pass, "CREATEDB NOSUPERUSER NOCREATEROLE").await?;
    }
    let runtime = role(&mut conn, &db.runtime_user).await?;
    if !runtime.exists {
        let pass = db
            .runtime_password
            .as_ref()
            .context("DB_RUNTIME_PASSWORD obrigatório para criar a role")?;
        create_role(&mut conn, &db.runtime_user, pass, "NOCREATEDB NOSUPERUSER NOCREATEROLE").await?;
    }
    check_roles(
        &role(&mut conn, &db.migrator_user).await?,
        &role(&mut conn, &db.runtime_user).await?,
        db,
    )?;

    let control = quote_ident(&db.control_db);
    if !database_exists(&mut conn, &db.control_db).await? {
        let sql = format!(
            "CREATE DATABASE {control} OWNER {} TEMPLATE template0 ENCODING 'UTF8'",
            quote_ident(&db.migrator_user)
        );
        match exec(&mut conn, sql).await {
            Ok(()) => tracing::info!(database = %db.control_db, "banco control criado"),
            Err(e) if db::pg_code(&e).as_deref() == Some("42P04") => {}
            Err(e) => return Err(e.into()),
        }
    }
    exec(
        &mut conn,
        format!(
            "REVOKE ALL ON DATABASE {control} FROM PUBLIC;
             GRANT CONNECT ON DATABASE {control} TO {}, {};",
            quote_ident(&db.migrator_user),
            quote_ident(&db.runtime_user)
        ),
    )
    .await?;
    conn.close().await?;
    tracing::info!("bootstrap concluído");
    Ok(())
}

/// Banco gerenciado: o DBA criou os recursos; só valida com o migrator.
pub async fn validate(db: &DbConfig) -> anyhow::Result<()> {
    let mut conn = db::connect("control", &db.migrator(&db.control_db)?, db.connect_retry)
        .await
        .context("o banco control precisa existir e aceitar o app_migrator")?;
    let migrator = role(&mut conn, &db.migrator_user).await?;
    let runtime = role(&mut conn, &db.runtime_user).await?;
    check_roles(&migrator, &runtime, db)?;
    conn.close().await?;
    tracing::info!("DB_BOOTSTRAP=false: roles e banco control validados");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Formato aceito pelo PostgreSQL. O login real com o verificador é
    /// coberto pelos testes de integração (o runtime conecta com a senha).
    #[test]
    fn scram_format() {
        let v = scram_verifier_with_salt("pencil", b"0123456789abcdef", 4096);
        assert!(v.starts_with("SCRAM-SHA-256$4096:MDEyMzQ1Njc4OWFiY2RlZg==$"), "{v}");
        assert!(v.chars().all(|c| c.is_ascii_alphanumeric() || "+/=:$-".contains(c)));
        assert_eq!(v.split('$').count(), 3);
    }
}
