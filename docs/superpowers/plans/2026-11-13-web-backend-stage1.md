# Etapa 1 do front web: pré-requisitos do backend — plano de implementação

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deixar a API pronta para o front: lista fechada de códigos de erro na especificação, `Retry-After` no 429,
IP real do cliente atrás de proxy confiável, origem de cada permissão em `/v1/users/{id}/access` e CI de testes.

**Architecture:** Mudanças pequenas e independentes na API Rust (Axum + sqlx). Cada tarefa tem testes de integração
contra PostgreSQL real (`tests/`) seguindo o padrão `World::new()` de `tests/common/mod.rs`. O CI reproduz
`make fmt lint sqlx-check test`.

**Tech Stack:** Rust 1.90 (edition 2024), Axum 0.8, sqlx 0.9 (cache offline em `.sqlx`), utoipa 5, moka,
GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-11-13-web-design.md` (seção "Etapas", item 1)

## Global Constraints

- Commits em Conventional Commits, em português, **sem** trailer `Co-Authored-By`.
- Toda query `sqlx::query!` nova ou alterada exige `make sqlx-prepare` e o commit do `.sqlx` atualizado.
- `make fmt`, `make lint` e `make test` passam ao final de cada tarefa (Postgres do projeto: `make db-up`).
- Mudanças só aditivas na API: nenhum campo existente muda de nome, tipo ou significado.
- README.md e README.pt-BR.md são atualizados juntos, com o mesmo conteúdo.
- Variáveis novas entram nos três `.env.*.example`, no `compose.yaml` (serviço `api`) e na tabela de variáveis dos
  READMEs.

## Review Focus

1. Cliente que manda `X-Forwarded-For` direto para a API (sem passar pelo proxy) não pode escolher o próprio IP:
   o header só vale quando o par TCP é um proxy confiável. (Task 3, `forged_forwarded_for_is_ignored`)
2. `X-Forwarded-For` com vários saltos, espaços ou lixo: usa o primeiro IP da direita que não é proxy confiável; se
   nada for um IP válido, usa o par TCP. (Task 3, testes unitários de `client_ip`)
3. Exceção expirada não aparece como origem de permissão, igual ao resolver. (Task 4,
   `sources_ignore_expired_overrides`)
4. Usuário inativo: nenhuma permissão efetiva, mas as origens continuam visíveis para o admin entender o cadastro.
   (Task 4, `inactive_user_has_no_effective_features`)
5. `Retry-After` nunca é 0 nem maior que a janela de login. (Task 2, `retry_after_is_within_window`)

---

### Task 1: Lista fechada de códigos de erro na especificação

**Files:**
- Modify: `src/error.rs` (macro dos códigos de campo, `reason::ALL`, `STATUS_CODES`)
- Modify: todos os chamadores de `Validator::err` e `AppError::invalid` (`src/access/handlers.rs`,
  `src/platform/handlers.rs`, `src/users/handlers.rs`, `src/users/model.rs`)
- Modify: `src/openapi.rs` (enum em `Problem.code` e `FieldError.code`)
- Test: `tests/errors.rs`, testes unitários em `src/error.rs`

**Interfaces:**
- Produces:
  - `pub struct FieldCode(&'static str)` com `pub const fn as_str(self) -> &'static str`; só construível dentro de
    `src/error.rs`.
  - `pub mod field` com uma constante por código e `pub const ALL: &[FieldCode]`, gerados pela mesma macro para que a
    lista nunca divirja das constantes. Códigos: `REQUIRED="required"`, `INVALID_EMAIL="invalid_email"`,
    `INVALID_LENGTH="invalid_length"`, `TOO_LONG="too_long"`, `INVALID_FORMAT="invalid_format"`,
    `INVALID_CHOICE="invalid_choice"`, `OUT_OF_RANGE="out_of_range"`, `TOO_MANY="too_many"`, `UNKNOWN="unknown"`,
    `MUST_BE_FUTURE="must_be_future"`, `NOT_ALLOWED="not_allowed"`, `NO_CHANGES="no_changes"`,
    `INVALID_BODY="invalid_body"`, `INVALID_QUERY="invalid_query"`.
  - `FieldError.code: FieldCode` (serializa como string). `Validator::err(&mut self, field: &'static str, code: FieldCode, message: &'static str)` e
    `AppError::invalid(field: &'static str, code: FieldCode, message: &'static str)`.
  - `reason::ALL: &[Reason]`, gerado pela mesma técnica de macro sobre as constantes existentes de `reason`.
  - `pub const STATUS_CODES: &[&str]` com todos os valores que `status_code()` pode devolver.
  - `pub fn problem_codes() -> Vec<&'static str>`: `STATUS_CODES` ∪ códigos de `reason::ALL`, sem repetição, ordenado.

- [ ] **Step 1: Escrever os testes que falham**

Em `tests/errors.rs`:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spec_lists_every_code_as_enum() {
    let spec: serde_json::Value = serde_json::from_str(&simple_core::openapi::spec_json().unwrap()).unwrap();
    let s = &spec["components"]["schemas"];
    let problem: Vec<&str> = s["Problem"]["properties"]["code"]["enum"].as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    let field: Vec<&str> = s["FieldError"]["properties"]["code"]["enum"].as_array().unwrap()
        .iter().map(|v| v.as_str().unwrap()).collect();
    for c in ["unauthorized", "not_found", "validation_failed", "rate_limited", "unavailable",
              "missing_feature", "cannot_grant", "email_taken", "last_tenant_admin"] {
        assert!(problem.contains(&c), "{c} fora do enum de Problem.code");
    }
    for c in ["required", "invalid_email", "invalid_length", "invalid_body", "out_of_range"] {
        assert!(field.contains(&c), "{c} fora do enum de FieldError.code");
    }
}
```

Em `src/error.rs` (`#[cfg(test)] mod tests`):
- `status_code_values_are_listed`: para cada `StatusCode` de 100 a 599 válido, `STATUS_CODES.contains(&status_code(s))`.
- `codes_are_unique_snake_case`: todo item de `field::ALL` e de `problem_codes()` casa com `^[a-z][a-z0-9_]*$`, e
  `field::ALL` não tem repetidos.

- [ ] **Step 2: Rodar e ver falhar**

Run: `make test` (ou `cargo test --locked --test errors spec_lists_every_code_as_enum`)
Expected: FAIL — `enum` ausente no schema; testes unitários não compilam (`STATUS_CODES`, `field::ALL` inexistentes).

- [ ] **Step 3: Implementar em `src/error.rs`**

Macro `macro_rules!` que recebe `NOME = "valor"` e gera as constantes e o `ALL`; usar a mesma ideia para `reason`
(nome, código, detalhe). Trocar os literais de código de campo pelos `field::*` em todos os chamadores (o compilador
aponta cada um, porque `code` passa a ser `FieldCode`).

- [ ] **Step 4: Implementar o enum na especificação (`src/openapi.rs`)**

No `Modify` (`AccessRules::modify` ou um modificador novo listado em `modifiers(...)`), substituir o schema de
`Problem.code` e `FieldError.code` por `string` com `enum` = `problem_codes()` e `field::ALL`, respectivamente.

- [ ] **Step 5: Rodar e ver passar**

Run: `make fmt lint test`
Expected: tudo PASS.

- [ ] **Step 6: README**

Na seção **Errors/Erros** dos dois READMEs, trocar "full list in `src/error.rs`" por: a lista completa está no
`enum` de `Problem.code` e `FieldError.code` da especificação (`make openapi`).

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(error): códigos de erro fechados e listados como enum na especificação"
```

---

### Task 2: `Retry-After` no 429

**Files:**
- Modify: `src/auth/ratelimit.rs` (contador guarda o instante de criação)
- Modify: `src/error.rs` (`TooManyRequests { retry_after_secs: u64 }` e header na resposta)
- Modify: `src/auth/handlers.rs` (documentar o header no `#[utoipa::path]` dos dois logins)
- Test: `tests/auth_limits.rs` (novo)

**Interfaces:**
- Produces: `AppError::TooManyRequests { retry_after_secs: u64 }`; a resposta 429 leva `Retry-After: <segundos>`.
- O valor é o tempo restante da janela da entrada que estourou (a maior das duas, IP ou e-mail), arredondado para
  cima, mínimo 1, máximo `login_window`.

- [ ] **Step 1: Escrever os testes que falham** (`tests/auth_limits.rs`)

Config de teste com `login_window = 60s` e `login_limit_platform = RateLimit { per_ip: 1000, per_email: 2 }`.

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rate_limited_login_has_retry_after() {
    // 3 logins com senha errada no mesmo e-mail: o 3º é 429
    // assert_eq!(r.status, 429); assert_eq!(r.body["code"], "rate_limited");
    // let secs: u64 = r.headers["retry-after"].to_str().unwrap().parse().unwrap();
    // assert!((1..=60).contains(&secs));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retry_after_is_within_window() {
    // window = 2s, per_email = 1: o 2º login dá 429 com Retry-After em 1..=2
}
```

- [ ] **Step 2: Rodar e ver falhar**

Run: `cargo test --locked --test auth_limits`
Expected: FAIL — header `retry-after` ausente.

- [ ] **Step 3: Implementar**

Em `LoginLimiter`, trocar `Cache<String, Arc<AtomicU32>>` por `Cache<String, Arc<Entry>>` com
`struct Entry { created: std::time::Instant, count: AtomicU32 }`; `hit` devolve `Result<(), Duration>` com o tempo
restante quando estoura. Em `IntoResponse`, `TooManyRequests` adiciona `header::RETRY_AFTER`.

- [ ] **Step 4: Rodar e ver passar**

Run: `make fmt lint test`
Expected: PASS.

- [ ] **Step 5: README** — na linha do rate limit ("Login rate limit… → 429"), acrescentar "with `Retry-After`".

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(auth): Retry-After no 429 do limite de login"
```

---

### Task 3: IP real do cliente atrás de proxy confiável

**Files:**
- Modify: `Cargo.toml` (dependência `ipnet`)
- Modify: `src/config.rs` (`trusted_proxies: Vec<ipnet::IpNet>`, variável `TRUSTED_PROXIES`)
- Modify: `src/auth/mod.rs` (`ClientIp` passa a ser `FromRequestParts<AppState>`; função pura `client_ip`)
- Modify: `tests/common/mod.rs` (`trusted_proxies: vec![]` na config)
- Modify: `.env.*.example`, `compose.yaml`, READMEs
- Test: `tests/auth_limits.rs`, testes unitários em `src/auth/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn client_ip(peer: IpAddr, forwarded_for: Option<&str>, trusted: &[IpNet]) -> IpAddr`
  - `ApiConfig.trusted_proxies: Vec<IpNet>`; `TRUSTED_PROXIES` é lista separada por vírgula de IPs ou CIDRs,
    padrão vazio (nenhum proxy confiável, comportamento atual). IP sem prefixo vira /32 ou /128. Valor inválido
    falha na inicialização com mensagem que cita a variável.
- Regra: se `peer` não está em `trusted`, devolve `peer` (header ignorado). Senão, percorre os IPs do
  `X-Forwarded-For` (todas as ocorrências do header, concatenadas na ordem) da direita para a esquerda e devolve o
  primeiro que não está em `trusted`; entradas que não são IP encerram a busca e devolvem `peer`; se todos forem
  confiáveis, devolve o mais à esquerda.

- [ ] **Step 1: Testes unitários que falham** (`src/auth/mod.rs`)

```rust
// trusted = ["10.0.0.0/8"]
// client_ip(203.0.113.9, Some("1.2.3.4"), &trusted) == 203.0.113.9          // par não confiável
// client_ip(10.0.0.2, Some("1.2.3.4"), &trusted) == 1.2.3.4
// client_ip(10.0.0.2, Some("1.2.3.4, 10.0.0.5"), &trusted) == 1.2.3.4       // vários saltos
// client_ip(10.0.0.2, Some(" 1.2.3.4 ,10.0.0.5 "), &trusted) == 1.2.3.4     // espaços
// client_ip(10.0.0.2, Some("lixo"), &trusted) == 10.0.0.2
// client_ip(10.0.0.2, None, &trusted) == 10.0.0.2
// client_ip(10.0.0.2, Some("2001:db8::1"), &trusted) == 2001:db8::1
```

- [ ] **Step 2: Teste de integração que falha** (`tests/auth_limits.rs`)

Requisições montadas com `Request::builder()` e `req.extensions_mut().insert(ConnectInfo(SocketAddr))`, enviadas por
`TestApp::send`. Config: `per_ip = 2`, `per_email = 1000`, `trusted_proxies = ["10.0.0.0/8"]`.

- `forwarded_for_from_trusted_proxy_is_used`: par `10.0.0.2`, três logins com `X-Forwarded-For` de IPs
  diferentes (`1.1.1.1`, `2.2.2.2`, `3.3.3.3`) → nenhum 429; três logins com o mesmo `1.1.1.1` → o 3º é 429.
- `forged_forwarded_for_is_ignored`: par `203.0.113.9` (não confiável), três logins com `X-Forwarded-For`
  diferentes → o 3º é 429 (o limite é do par).

- [ ] **Step 3: Rodar e ver falhar**

Run: `cargo test --locked --lib auth:: && cargo test --locked --test auth_limits`
Expected: FAIL — `client_ip` e `trusted_proxies` inexistentes.

- [ ] **Step 4: Implementar** — `client_ip` puro; o extractor lê `ConnectInfo` (ausente → `0.0.0.0`, como hoje),
  os headers `x-forwarded-for` e `state.cfg.trusted_proxies`.

- [ ] **Step 5: Rodar e ver passar**

Run: `make fmt lint test`
Expected: PASS.

- [ ] **Step 6: Configuração e docs** — `TRUSTED_PROXIES=` (vazio) nos três `.env.*.example` com comentário
  "IPs/CIDRs de proxies cujo X-Forwarded-For é aceito (ex.: o Nginx do front)"; `TRUSTED_PROXIES:
  ${TRUSTED_PROXIES:-}` no serviço `api` do `compose.yaml`; linha na tabela de variáveis e uma frase na seção de
  rate limit dos READMEs.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(auth): IP do cliente via X-Forwarded-For de proxies confiáveis"
```

---

### Task 4: Origem de cada permissão em `/v1/users/{id}/access`

**Files:**
- Modify: `src/access/resolver.rs` (query das origens)
- Modify: `src/access/handlers.rs` (`UserAccess.sources`)
- Modify: `.sqlx/` (via `make sqlx-prepare`)
- Test: `tests/access.rs`

**Interfaces:**
- Produces:
  - `pub async fn feature_sources<'e, E: PgExecutor<'e>>(exec: E, user_id: Uuid) -> Result<Vec<SourceRow>, sqlx::Error>`
    com `pub struct SourceRow { pub feature: String, pub kind: String /* "profile" | "allow" | "deny" */, pub profile_id: Option<Uuid> }`.
    Ignora exceções expiradas (`expires_at IS NULL OR expires_at > now()`), igual a `effective_features`.
  - `UserAccess.sources: Vec<FeatureSource>`, ordenado pelo código da feature, uma entrada por feature com alguma
    origem (para admin: todas as features do catálogo):

```rust
#[derive(Serialize, ToSchema)]
pub struct FeatureSource {
    pub feature: String,
    /// Igual a estar em `effective_features`.
    pub effective: bool,
    /// O que concede a feature.
    pub granted_by: Vec<Grant>,
    /// Há um `deny` válido (deny sempre vence).
    pub denied: bool,
}

#[derive(Serialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Grant {
    Profile { profile_id: Uuid },
    Allow,
    /// Admin do tenant: acesso total.
    Admin,
}
```

- [ ] **Step 1: Testes que falham** (`tests/access.rs`, usando os helpers existentes `create_profile`,
  `set_profiles`, override por `PUT /v1/users/{id}/overrides/{code}`)

- `access_lists_sources_per_feature`: perfil P com `users.read` e `users.create`; `allow` em `access.read`;
  `deny` em `users.create`. Esperado em `sources`:
  - `users.read`: `effective=true`, `granted_by=[{kind:"profile", profile_id:P}]`, `denied=false`
  - `users.create`: `effective=false`, `granted_by=[{kind:"profile", profile_id:P}]`, `denied=true`
  - `access.read`: `effective=true`, `granted_by=[{kind:"allow"}]`, `denied=false`
- `admin_sources_are_admin`: para o admin do seed, toda entrada tem `granted_by` contendo `{kind:"admin"}` e
  `effective=true`; o número de entradas é igual ao do catálogo (`GET /v1/modules`).
- `sources_ignore_expired_overrides`: `allow` com `expires_at` no passado inserido direto no banco do tenant (a API
  recusa expiração passada) → a feature não aparece em `sources`.
- `inactive_user_has_no_effective_features`: usuário com perfil, desativado → toda entrada com `effective=false`,
  `granted_by` preservado.

- [ ] **Step 2: Rodar e ver falhar**

Run: `cargo test --locked --test access sources`
Expected: FAIL — campo `sources` ausente.

- [ ] **Step 3: Implementar** — uma query com `UNION ALL` (perfis, allows válidos, denys válidos); montagem em Rust
  junto com `effective_features` e o flag de admin. Rodar `make sqlx-prepare`.

- [ ] **Step 4: Rodar e ver passar**

Run: `make fmt lint sqlx-check test`
Expected: PASS.

- [ ] **Step 5: README** — no item de testes de acesso, acrescentar "per-feature sources (profile, allow, admin,
  deny)".

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(access): origem de cada permissão em /v1/users/{id}/access"
```

---

### Task 5: CI de testes da API

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: READMEs (seção de desenvolvimento: o que o CI roda) e `scripts/new-project.sh` (README gerado menciona o CI)

**Interfaces:**
- Consumes: alvos `make fmt`, `make lint`, `make db-up`, `make sqlx-check`, `make test`.

- [ ] **Step 1: Escrever o workflow**

Gatilhos: `push` em `main` e `pull_request`. Um job `api` em `ubuntu-latest`:
1. `actions/checkout`; Rust estável (mesma forma do `release.yml`) com `rustfmt` e `clippy`;
   `Swatinem/rust-cache@v2`; `cargo install sqlx-cli --version ^0.9 --no-default-features --features postgres --locked`.
2. Gerar `.env.local` a partir de `.env.local.example`, preenchendo cada variável vazia de senha/segredo com
   `openssl rand -hex 32` e os `SEED_*` com valores de teste (`ci@example.com`, `CI`, senha aleatória, `ci`, `CI`).
3. `make fmt`, `make lint`, `make db-up`, `make sqlx-check`, `make test`.

- [ ] **Step 2: Validar localmente o passo de geração do `.env.local`**

Rodar o mesmo script em um diretório temporário e conferir com `bash scripts/with-env.sh <arquivo> env` que nenhuma
variável obrigatória ficou vazia.

- [ ] **Step 3: Commit, push e conferir o run**

```bash
git add -A
git commit -m "ci: testes da API (fmt, clippy, sqlx-check, testes com PostgreSQL)"
git push
gh run watch --exit-status
```

Expected: job `api` verde.
