//! Catálogo de módulos e features, definido no código e sincronizado em cada
//! banco de tenant. A API só o lê.

use sqlx::PgConnection;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Feature {
    UsersRead = 0,
    UsersCreate,
    UsersUpdate,
    UsersDelete,
    AccessRead,
    AccessProfilesManage,
    AccessUsersManage,
}

pub struct ModuleDef {
    pub code: &'static str,
    pub name: &'static str,
}

pub struct FeatureDef {
    pub feature: Feature,
    pub module: &'static str,
    pub code: &'static str,
    pub name: &'static str,
}

pub const MODULES: &[ModuleDef] = &[
    ModuleDef {
        code: "users",
        name: "Usuários",
    },
    ModuleDef {
        code: "access",
        name: "Controle de acesso",
    },
];

pub const FEATURES: &[FeatureDef] = &[
    FeatureDef {
        feature: Feature::UsersRead,
        module: "users",
        code: "users.read",
        name: "Consultar usuários",
    },
    FeatureDef {
        feature: Feature::UsersCreate,
        module: "users",
        code: "users.create",
        name: "Criar usuários",
    },
    FeatureDef {
        feature: Feature::UsersUpdate,
        module: "users",
        code: "users.update",
        name: "Alterar usuários",
    },
    FeatureDef {
        feature: Feature::UsersDelete,
        module: "users",
        code: "users.delete",
        name: "Excluir usuários",
    },
    FeatureDef {
        feature: Feature::AccessRead,
        module: "access",
        code: "access.read",
        name: "Consultar acessos",
    },
    FeatureDef {
        feature: Feature::AccessProfilesManage,
        module: "access",
        code: "access.profiles.manage",
        name: "Gerenciar perfis",
    },
    FeatureDef {
        feature: Feature::AccessUsersManage,
        module: "access",
        code: "access.users.manage",
        name: "Gerenciar acessos de usuários",
    },
];

impl Feature {
    pub fn code(self) -> &'static str {
        FEATURES[self as usize].code
    }

    pub fn from_code(code: &str) -> Option<Self> {
        FEATURES.iter().find(|f| f.code == code).map(|f| f.feature)
    }
}

/// Conjunto de features como bitset: verificação O(1) e cópia barata.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct FeatureSet(u64);

impl FeatureSet {
    pub const fn empty() -> Self {
        Self(0)
    }

    pub fn all() -> Self {
        Self(FEATURES.iter().fold(0, |acc, f| acc | (1 << f.feature as u8)))
    }

    pub fn insert(&mut self, f: Feature) {
        self.0 |= 1 << f as u8;
    }

    #[inline]
    pub fn contains(self, f: Feature) -> bool {
        self.0 & (1 << f as u8) != 0
    }

    pub fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    pub fn is_subset_of(self, other: Self) -> bool {
        self.0 & !other.0 == 0
    }

    pub fn from_codes<I: IntoIterator<Item = S>, S: AsRef<str>>(codes: I) -> Self {
        let mut s = Self::empty();
        for c in codes {
            if let Some(f) = Feature::from_code(c.as_ref()) {
                s.insert(f);
            }
        }
        s
    }

    pub fn codes(self) -> Vec<&'static str> {
        FEATURES
            .iter()
            .filter(|f| self.contains(f.feature))
            .map(|f| f.code)
            .collect()
    }
}

/// Sincroniza o catálogo pelo `code`, numa transação. Nunca remove features
/// (expand/contract): features antigas podem estar referenciadas.
pub async fn sync(conn: &mut PgConnection) -> Result<(), sqlx::Error> {
    use sqlx::Connection;
    let mut tx = conn.begin().await?;
    let m_codes: Vec<String> = MODULES.iter().map(|m| m.code.to_owned()).collect();
    let m_names: Vec<String> = MODULES.iter().map(|m| m.name.to_owned()).collect();
    sqlx::query(
        "INSERT INTO modules (code, name)
         SELECT code, name FROM unnest($1::text[], $2::text[]) AS m(code, name)
         ON CONFLICT (code) DO UPDATE SET name = EXCLUDED.name
         WHERE modules.name IS DISTINCT FROM EXCLUDED.name",
    )
    .bind(&m_codes)
    .bind(&m_names)
    .execute(&mut *tx)
    .await?;

    let f_modules: Vec<String> = FEATURES.iter().map(|f| f.module.to_owned()).collect();
    let f_codes: Vec<String> = FEATURES.iter().map(|f| f.code.to_owned()).collect();
    let f_names: Vec<String> = FEATURES.iter().map(|f| f.name.to_owned()).collect();
    sqlx::query(
        "INSERT INTO features (module_id, code, name)
         SELECT m.id, f.code, f.name
         FROM unnest($1::text[], $2::text[], $3::text[]) AS f(module_code, code, name)
         JOIN modules m ON m.code = f.module_code
         ON CONFLICT (code) DO UPDATE SET name = EXCLUDED.name, module_id = EXCLUDED.module_id
         WHERE features.name IS DISTINCT FROM EXCLUDED.name
            OR features.module_id IS DISTINCT FROM EXCLUDED.module_id",
    )
    .bind(&f_modules)
    .bind(&f_codes)
    .bind(&f_names)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        for (i, f) in FEATURES.iter().enumerate() {
            assert_eq!(f.feature as usize, i, "ordem do enum deve bater com FEATURES");
            assert!(
                f.code.starts_with(&format!("{}.", f.module)),
                "código deve ser modulo.acao"
            );
            assert!(MODULES.iter().any(|m| m.code == f.module));
            assert_eq!(Feature::from_code(f.code), Some(f.feature));
        }
        assert!(FEATURES.len() <= 64);
    }

    #[test]
    fn feature_set_ops() {
        let s = FeatureSet::from_codes(["users.read", "access.read", "nao.existe"]);
        assert!(s.contains(Feature::UsersRead));
        assert!(!s.contains(Feature::UsersDelete));
        assert!(s.is_subset_of(FeatureSet::all()));
        assert!(!FeatureSet::all().is_subset_of(s));
        assert_eq!(s.codes(), vec!["users.read", "access.read"]);
    }
}
