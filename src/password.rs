//! Argon2id (m=19456, t=2, p=1) fora do runtime assíncrono.
//!
//! Cada hash/verificação roda em `spawn_blocking`, limitado por um `Semaphore`
//! do tamanho do número de núcleos. Se a vaga não sair em 1 s, retorna 503.

use std::sync::Arc;
use std::time::Duration;

use argon2::{Algorithm, Argon2, Params, PasswordHasher as _, PasswordVerifier as _, Version};
use tokio::sync::Semaphore;

use crate::error::{AppError, AppResult};

pub const M_COST: u32 = 19_456;
pub const T_COST: u32 = 2;
pub const P_COST: u32 = 1;

fn argon() -> Argon2<'static> {
    let params = Params::new(M_COST, T_COST, P_COST, None).expect("parâmetros argon2 válidos");
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
}

/// Hash síncrono (CLI). Nunca chamar no runtime assíncrono.
pub fn hash_blocking(password: &str) -> anyhow::Result<String> {
    argon()
        .hash_password(password.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("falha ao gerar hash: {e}"))
}

fn verify_blocking(password: &str, hash: &str) -> bool {
    argon().verify_password(password.as_bytes(), hash).is_ok()
}

#[derive(Clone)]
pub struct PasswordHasher {
    sem: Arc<Semaphore>,
    wait: Duration,
    /// Hash de uma senha aleatória, usado quando o usuário não existe para
    /// igualar o tempo de resposta.
    dummy: Arc<str>,
}

impl PasswordHasher {
    pub fn new(concurrency: usize, wait: Duration) -> anyhow::Result<Self> {
        let mut rnd = [0u8; 24];
        rand::fill(&mut rnd);
        let dummy = hash_blocking(&crate::auth::b64(&rnd))?;
        Ok(Self {
            sem: Arc::new(Semaphore::new(concurrency)),
            wait,
            dummy: dummy.into(),
        })
    }

    async fn run<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> AppResult<T> {
        let gauge = metrics::gauge!("password_hash_queue");
        gauge.increment(1.0);
        let permit = tokio::time::timeout(self.wait, self.sem.clone().acquire_owned()).await;
        gauge.decrement(1.0);
        let permit = match permit {
            Ok(Ok(p)) => p,
            _ => {
                metrics::counter!("password_hash_rejected_total").increment(1);
                return Err(AppError::Unavailable("password hashing capacity exhausted"));
            }
        };
        let started = std::time::Instant::now();
        let out = tokio::task::spawn_blocking(move || {
            let r = f();
            drop(permit);
            r
        })
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!("tarefa de hash falhou: {e}")))?;
        metrics::histogram!("password_hash_duration_seconds").record(started.elapsed().as_secs_f64());
        Ok(out)
    }

    pub async fn hash(&self, password: String) -> AppResult<String> {
        self.run(move || hash_blocking(&password))
            .await?
            .map_err(AppError::Internal)
    }

    /// Verifica a senha. Sem hash (usuário inexistente), verifica contra o hash
    /// fictício e retorna `false`, gastando o mesmo tempo.
    pub async fn verify(&self, password: String, hash: Option<String>) -> AppResult<bool> {
        let dummy = self.dummy.clone();
        self.run(move || match hash {
            Some(h) => verify_blocking(&password, &h),
            None => {
                let _ = verify_blocking(&password, &dummy);
                false
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_uses_required_params() {
        let h = hash_blocking("uma senha longa o bastante").unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
        assert!(verify_blocking("uma senha longa o bastante", &h));
        assert!(!verify_blocking("outra senha", &h));
    }
}
