//! Modelo e validação de usuários (tenant e plataforma usam as mesmas regras).

use serde::Serialize;
use time::OffsetDateTime;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::error::{AppError, FieldError};

pub const EMAIL_MAX: usize = 254;
pub const NAME_MAX: usize = 120;
pub const PASSWORD_MIN: usize = 12;
pub const PASSWORD_MAX: usize = 128;

#[derive(Default)]
pub struct Validator(Vec<FieldError>);

impl Validator {
    pub fn err(&mut self, field: &'static str, code: &'static str, message: &'static str) {
        self.0.push(FieldError { field, code, message });
    }

    pub fn finish(self) -> Result<(), AppError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(AppError::Validation(self.0))
        }
    }

    pub fn email(&mut self, field: &'static str, raw: &str) -> String {
        let e = raw.trim().to_lowercase();
        let valid = e.chars().count() <= EMAIL_MAX
            && !e.chars().any(|c| c.is_whitespace() || c.is_control())
            && matches!(e.split_once('@'), Some((l, d)) if !l.is_empty() && !d.is_empty() && !d.contains('@'));
        if !valid {
            self.err(
                field,
                "invalid_email",
                "must be a valid e-mail with at most 254 characters",
            );
        }
        e
    }

    pub fn name(&mut self, field: &'static str, raw: &str) -> String {
        let n = raw.trim().to_owned();
        let len = n.chars().count();
        if !(1..=NAME_MAX).contains(&len) || n.chars().any(char::is_control) {
            self.err(field, "invalid_length", "must have between 1 and 120 characters");
        }
        n
    }

    pub fn password(&mut self, field: &'static str, raw: &str) {
        let len = raw.chars().count();
        if !(PASSWORD_MIN..=PASSWORD_MAX).contains(&len) {
            self.err(field, "invalid_length", "must have between 12 and 128 characters");
        }
    }
}

/// Normaliza e-mail de login sem rejeitar (o login responde sempre 401 genérico).
pub fn login_email(raw: &str) -> String {
    raw.trim().to_lowercase()
}

#[derive(Debug, Serialize, ToSchema)]
pub struct UserOut {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub is_tenant_admin: bool,
    pub is_active: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// Cursor da próxima página (`after`); `null` na última.
    pub next_after: Option<Uuid>,
}

/// `limit` padrão 50, máximo 100.
pub fn page_limit(limit: Option<i64>) -> Result<i64, AppError> {
    match limit {
        None => Ok(50),
        Some(l) if (1..=100).contains(&l) => Ok(l),
        Some(_) => Err(AppError::invalid("limit", "out_of_range", "must be between 1 and 100")),
    }
}

pub fn page<T>(items: Vec<T>, limit: i64, id: impl Fn(&T) -> Uuid) -> Page<T> {
    let next_after = if items.len() as i64 == limit {
        items.last().map(&id)
    } else {
        None
    };
    Page { items, next_after }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_normalization() {
        let mut v = Validator::default();
        assert_eq!(v.email("email", "  Foo@Example.COM "), "foo@example.com");
        assert!(v.finish().is_ok());
        let mut v = Validator::default();
        v.email("email", "sem-arroba");
        v.email("email", "a b@c.d");
        v.email("email", &format!("{}@x.io", "a".repeat(250)));
        assert!(matches!(v.finish(), Err(AppError::Validation(e)) if e.len() == 3));
    }

    #[test]
    fn name_and_password() {
        let mut v = Validator::default();
        v.name("name", "");
        v.name("name", &"x".repeat(121));
        v.password("password", "curta");
        v.password("password", &"x".repeat(129));
        assert!(matches!(v.finish(), Err(AppError::Validation(e)) if e.len() == 4));
        let mut v = Validator::default();
        v.name("name", "Ana");
        v.password("password", "doze-chars-ok");
        assert!(v.finish().is_ok());
    }
}
