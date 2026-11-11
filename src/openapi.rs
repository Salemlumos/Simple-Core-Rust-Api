//! Documentação OpenAPI 3.1 gerada a partir do código (utoipa) e Swagger UI.
//!
//! Os handlers declaram caminho, corpo e respostas de sucesso. A regra de
//! acesso de cada rota (token exigido, feature, `X-Tenant-Slug`) e as respostas
//! de erro comuns são aplicadas por [`AccessRules`] a partir de
//! [`crate::app::route_defs`], a mesma lista que monta o roteador; assim a
//! documentação não diverge da autorização real. Um teste garante que toda rota
//! está documentada e que nada além delas aparece.
//!
//! A especificação é gerada uma vez, na montagem do roteador, e só é servida
//! com `API_DOCS=true`; nada disso passa pelo caminho das requisições da API.

use axum::Router;
use axum::http::Method;
use utoipa::openapi::path::{Operation, ParameterBuilder, ParameterIn, PathItem};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::openapi::{ContentBuilder, ObjectBuilder, Ref, Required, ResponseBuilder, Type};
use utoipa::{Modify, OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::access::handlers as access;
use crate::auth::handlers as auth;
use crate::auth::middleware::Access;
use crate::error::FieldError;
use crate::platform::handlers as platform;
use crate::tenancy::middleware::TENANT_HEADER;
use crate::users::handlers as users;

/// Caminho da interface Swagger UI.
pub const DOCS_PATH: &str = "/docs";
/// Caminho da especificação em JSON.
pub const SPEC_PATH: &str = "/openapi.json";

const PLATFORM_TOKEN: &str = "platform_token";
const TENANT_TOKEN: &str = "tenant_token";
const PROBLEM_JSON: &str = "application/problem+json";

/// Corpo de erro RFC 9457 (`application/problem+json`).
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct Problem {
    /// Sempre `about:blank`.
    #[schema(example = "about:blank")]
    r#type: String,
    /// Frase padrão do status HTTP.
    #[schema(example = "Forbidden")]
    title: String,
    #[schema(example = 403)]
    status: u16,
    /// Código estável do erro, para decidir e traduzir mensagens no cliente.
    /// Sempre presente: o padrão do status (`unauthorized`, `not_found`,
    /// `validation_failed`, `rate_limited`, `unavailable`...) ou um motivo
    /// específico de 403/409 (`missing_feature`, `cannot_grant`, `email_taken`...).
    #[schema(example = "missing_feature")]
    code: String,
    /// Explicação curta; nunca contém senha, hash ou token.
    #[schema(example = "missing feature users.update")]
    detail: Option<String>,
    /// Erros por campo (somente em 422).
    errors: Option<Vec<FieldError>>,
}

/// Resposta dos health checks.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct Health {
    #[schema(example = "ok")]
    status: String,
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "simple-core",
        description = "API multi-tenant de usuários com controle de acesso.\n\n\
            **Como autenticar no Swagger:** faça login em `POST /v1/auth/login` (usuário de tenant, \
            com o header `X-Tenant-Slug`) ou em `POST /v1/platform/auth/login` (owner ou superusuário), \
            copie o `access_token` e cole em **Authorize** no esquema correspondente. \
            Com token de plataforma, as rotas de tenant exigem também o header `X-Tenant-Slug`.\n\n\
            Erros seguem a RFC 9457 (`application/problem+json`)."
    ),
    paths(
        crate::app::live,
        crate::app::ready,
        auth::tenant_login,
        auth::tenant_refresh,
        auth::tenant_logout,
        auth::tenant_me,
        auth::platform_login,
        auth::platform_refresh,
        auth::platform_logout,
        platform::me,
        platform::patch_me,
        platform::list_tenants,
        platform::get_tenant,
        platform::create_tenant,
        platform::patch_tenant,
        platform::retry_tenant,
        platform::list_superusers,
        platform::create_superuser,
        platform::patch_superuser,
        platform::delete_superuser,
        platform::list_admins,
        platform::create_admin,
        platform::delete_admin,
        users::create,
        users::list,
        users::get,
        users::patch,
        users::delete,
        access::modules,
        access::list_profiles,
        access::get_profile,
        access::create_profile,
        access::patch_profile,
        access::delete_profile,
        access::put_profile_features,
        access::user_access,
        access::put_user_profiles,
        access::put_override,
        access::delete_override,
    ),
    components(schemas(Problem, FieldError)),
    modifiers(&AccessRules),
    tags(
        (name = "saúde", description = "Health checks (sem autenticação)"),
        (name = "autenticação", description = "Login, refresh e logout de usuários de tenant"),
        (name = "usuários", description = "Usuários do tenant"),
        (name = "acesso", description = "Catálogo de módulos e features"),
        (name = "acesso: perfis", description = "Perfis (conjuntos de features)"),
        (name = "acesso: usuários", description = "Perfis e exceções (allow/deny) de cada usuário"),
        (name = "plataforma: autenticação", description = "Login, refresh e logout de owner e superusuários"),
        (name = "plataforma", description = "Usuário de plataforma logado"),
        (name = "plataforma: tenants", description = "Ciclo de vida dos tenants"),
        (name = "plataforma: superusuários", description = "Somente o owner"),
        (name = "plataforma: admins de tenant", description = "Somente o owner"),
    )
)]
pub struct ApiDoc;

/// Especificação em JSON formatado (`api openapi`), a mesma servida em [`SPEC_PATH`].
pub fn spec_json() -> anyhow::Result<String> {
    Ok(ApiDoc::openapi().to_pretty_json()?)
}

/// Swagger UI em [`DOCS_PATH`] e a especificação em [`SPEC_PATH`].
pub fn docs_router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    SwaggerUi::new(DOCS_PATH).url(SPEC_PATH, ApiDoc::openapi()).into()
}

/// Aplica segurança, `X-Tenant-Slug` e respostas de erro comuns conforme o
/// [`Access`] declarado em cada rota.
struct AccessRules;

impl Modify for AccessRules {
    fn modify(&self, api: &mut utoipa::openapi::OpenApi) {
        // Sem `license` no Cargo.toml o utoipa gera um objeto vazio; omite.
        if api.info.license.as_ref().is_some_and(|l| l.name.is_empty()) {
            api.info.license = None;
        }
        let components = api.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            PLATFORM_TOKEN,
            bearer("Access token de `POST /v1/platform/auth/login` (owner ou superusuário)."),
        );
        components.add_security_scheme(
            TENANT_TOKEN,
            bearer("Access token de `POST /v1/auth/login` (usuário de tenant)."),
        );
        for def in crate::app::route_defs() {
            let op = api
                .paths
                .paths
                .get_mut(def.path)
                .and_then(|item| operation_mut(item, &def.method));
            if let Some(op) = op {
                op.operation_id = Some(operation_id(&def.method, def.path));
                apply_access(op, def.path, def.access);
            }
        }
    }
}

fn bearer(description: &str) -> SecurityScheme {
    SecurityScheme::Http(
        HttpBuilder::new()
            .scheme(HttpAuthScheme::Bearer)
            .bearer_format("JWT")
            .description(Some(description))
            .build(),
    )
}

fn operation_mut<'a>(item: &'a mut PathItem, method: &Method) -> Option<&'a mut Operation> {
    match *method {
        Method::GET => item.get.as_mut(),
        Method::POST => item.post.as_mut(),
        Method::PUT => item.put.as_mut(),
        Method::PATCH => item.patch.as_mut(),
        Method::DELETE => item.delete.as_mut(),
        _ => None,
    }
}

/// Id único derivado de método e caminho: `PATCH /v1/users/{id}` vira
/// `patch_users_id`. Os nomes dos handlers se repetem entre módulos.
fn operation_id(method: &Method, path: &str) -> String {
    let mut id = method.as_str().to_ascii_lowercase();
    for seg in path.split('/').filter(|s| !s.is_empty() && *s != "v1") {
        id.push('_');
        id.extend(
            seg.chars()
                .filter(|c| *c != '{' && *c != '}')
                .map(|c| if c == '-' { '_' } else { c }),
        );
    }
    id
}

fn requirement(scheme: &str) -> SecurityRequirement {
    SecurityRequirement::new(scheme, Vec::<String>::new())
}

fn apply_access(op: &mut Operation, path: &str, access: Access) {
    let tenant_route = matches!(access, Access::TenantToken | Access::Feature(_) | Access::SelfOr(_));
    let rule = match access {
        Access::Public => "público, sem token.".to_owned(),
        Access::Platform => "token de plataforma (owner ou superusuário ativo).".to_owned(),
        Access::PlatformOwner => "somente o owner (token de plataforma).".to_owned(),
        Access::TenantToken => "token de tenant de um usuário ativo (tokens de plataforma recebem 403).".to_owned(),
        Access::Feature(f) => format!(
            "feature `{}`. Admins do tenant, owner e superusuários sempre passam.",
            f.code()
        ),
        Access::SelfOr(f) => format!(
            "feature `{}`, ou o próprio usuário (com as restrições descritas acima). \
             Admins do tenant, owner e superusuários sempre passam.",
            f.code()
        ),
    };
    let text = format!("**Acesso:** {rule}");
    op.description = Some(match op.description.take() {
        Some(d) if !d.is_empty() => format!("{d}\n\n{text}"),
        _ => text,
    });

    op.security = match access {
        Access::Public => None,
        Access::Platform | Access::PlatformOwner => Some(vec![requirement(PLATFORM_TOKEN)]),
        Access::TenantToken => Some(vec![requirement(TENANT_TOKEN)]),
        // Alternativas: token de tenant OU token de plataforma (+ X-Tenant-Slug).
        Access::Feature(_) | Access::SelfOr(_) => Some(vec![requirement(TENANT_TOKEN), requirement(PLATFORM_TOKEN)]),
    };

    if tenant_route {
        let header = ParameterBuilder::new()
            .name("X-Tenant-Slug")
            .parameter_in(ParameterIn::Header)
            .required(Required::False)
            .description(Some(
                "Slug do tenant. Obrigatório com token de plataforma; com token de tenant, \
                 se enviado, precisa ser o tenant do token.",
            ))
            .schema(Some(ObjectBuilder::new().schema_type(Type::String)))
            .build();
        debug_assert!(TENANT_HEADER.eq_ignore_ascii_case("X-Tenant-Slug"));
        op.parameters.get_or_insert_with(Vec::new).push(header.into());
    }

    let mut add = |status: &str, description: &str| {
        op.responses
            .responses
            .entry(status.to_owned())
            .or_insert_with(|| problem_response(description));
    };
    if !access.is_public() {
        add("401", "Token ausente, inválido ou expirado");
        add(
            "403",
            if tenant_route {
                "Sem permissão, tenant suspenso ou `X-Tenant-Slug` diferente do tenant do token"
            } else {
                "Sem permissão"
            },
        );
    }
    let has_body = op.request_body.is_some();
    let has_query = op
        .parameters
        .iter()
        .flatten()
        .any(|p| matches!(p, utoipa::openapi::RefOr::T(p) if p.parameter_in == ParameterIn::Query));
    if has_body {
        add("400", "JSON malformado");
        add("413", "Corpo maior que 16 KiB");
        add("415", "Content-Type diferente de application/json");
    }
    if has_body || has_query {
        add("422", "Validação falhou (detalhes em `errors`)");
    }
    if path.contains('{') {
        add("404", "Recurso não encontrado");
    }
    add("500", "Erro interno (sem detalhes na resposta)");
    add(
        "503",
        "Banco indisponível, pool de conexões esgotado ou tempo limite excedido",
    );
}

fn problem_response(description: &str) -> utoipa::openapi::RefOr<utoipa::openapi::Response> {
    ResponseBuilder::new()
        .description(description)
        .content(
            PROBLEM_JSON,
            ContentBuilder::new()
                .schema(Some(Ref::from_schema_name("Problem")))
                .build(),
        )
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use utoipa::openapi::RefOr;
    use utoipa::openapi::path::{Operation, PathItem};

    use super::*;

    fn operations(item: &PathItem) -> Vec<(&'static str, &Operation)> {
        [
            ("GET", &item.get),
            ("POST", &item.post),
            ("PUT", &item.put),
            ("PATCH", &item.patch),
            ("DELETE", &item.delete),
            ("HEAD", &item.head),
            ("OPTIONS", &item.options),
            ("TRACE", &item.trace),
        ]
        .into_iter()
        .filter_map(|(m, op)| op.as_ref().map(|op| (m, op)))
        .collect()
    }

    /// Toda rota do roteador está documentada, e nada além delas.
    #[test]
    fn spec_matches_router() {
        let api = ApiDoc::openapi();
        let documented: BTreeSet<(String, String)> = api
            .paths
            .paths
            .iter()
            .flat_map(|(path, item)| operations(item).into_iter().map(|(m, _)| (m.to_owned(), path.clone())))
            .collect();
        let routed: BTreeSet<(String, String)> = crate::app::route_defs()
            .iter()
            .map(|d| (d.method.to_string(), d.path.to_owned()))
            .collect();
        assert_eq!(documented, routed);
    }

    /// Segurança e parâmetros de caminho coerentes com o `Access` e com o padrão da rota.
    #[test]
    fn security_and_path_params_follow_route_defs() {
        let api = ApiDoc::openapi();
        for d in crate::app::route_defs() {
            let op = operations(api.paths.get_path_item(d.path).unwrap())
                .into_iter()
                .find(|(m, _)| *m == d.method.as_str())
                .unwrap()
                .1;
            let schemes: BTreeSet<String> = op
                .security
                .iter()
                .flatten()
                .flat_map(|r| {
                    serde_json::to_value(r)
                        .unwrap()
                        .as_object()
                        .unwrap()
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .collect();
            let expected: BTreeSet<String> = match d.access {
                Access::Public => BTreeSet::new(),
                Access::Platform | Access::PlatformOwner => [PLATFORM_TOKEN.to_owned()].into(),
                Access::TenantToken => [TENANT_TOKEN.to_owned()].into(),
                Access::Feature(_) | Access::SelfOr(_) => [TENANT_TOKEN.to_owned(), PLATFORM_TOKEN.to_owned()].into(),
            };
            assert_eq!(schemes, expected, "{} {}", d.method, d.path);

            let path_params: BTreeSet<String> = op
                .parameters
                .iter()
                .flatten()
                .filter_map(|p| match p {
                    RefOr::T(p) if p.parameter_in == ParameterIn::Path => Some(p.name.clone()),
                    _ => None,
                })
                .collect();
            let placeholders: BTreeSet<String> = d
                .path
                .split('/')
                .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
                .map(str::to_owned)
                .collect();
            assert_eq!(path_params, placeholders, "{} {}", d.method, d.path);
            assert!(op.responses.responses.contains_key("500"), "{} {}", d.method, d.path);
        }
    }

    #[test]
    fn operation_ids_are_unique() {
        let api = ApiDoc::openapi();
        let ids: Vec<String> = api
            .paths
            .paths
            .values()
            .flat_map(|item| {
                operations(item)
                    .into_iter()
                    .map(|(_, op)| op.operation_id.clone().unwrap())
            })
            .collect();
        let unique: BTreeSet<&String> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "{ids:?}");
        assert!(ids.contains(&"patch_users_id".to_owned()));
    }

    #[test]
    fn problem_schema_uses_rfc9457_field_names() {
        let json = serde_json::to_value(ApiDoc::openapi()).unwrap();
        let props = &json["components"]["schemas"]["Problem"]["properties"];
        for k in ["type", "title", "status", "detail", "errors"] {
            assert!(props.get(k).is_some(), "falta {k}: {props}");
        }
    }
}
