# simple-core

[English](README.md) | **Português**

API REST multi-tenant de usuários com controle de acesso, focada em baixa latência e alto throughput.

- Rust estável (1.99), Tokio, Axum 0.8, Tower
- PostgreSQL 18 com SQLx 0.9 (queries verificadas em compilação, modo offline com `.sqlx`)
- `moka` (caches), `argon2` (Argon2id), `jsonwebtoken` (HS256), `tracing` (JSON não bloqueante), métricas Prometheus
- OpenAPI 3.1 gerada do código (`utoipa`) com Swagger UI em `/docs`
- Docker Compose (perfis `db` e `app`) e k6 para carga

Um único binário, `api`, com os comandos `bootstrap`, `migrate`, `seed`, `serve`, `worker` e `healthcheck`.

---

## Sumário

1. [Execução](#execução)
2. [Documentação da API (Swagger)](#documentação-da-api-swagger)
3. [Variáveis de ambiente](#variáveis-de-ambiente)
4. [Banco gerenciado (DB_BOOTSTRAP=false)](#banco-gerenciado-db_bootstrapfalse)
5. [Migrar do banco em container para um banco externo](#migrar-do-banco-em-container-para-um-banco-externo)
6. [Arquitetura e decisões técnicas](#arquitetura-e-decisões-técnicas)
7. [Testes](#testes)
8. [Testes de carga](#testes-de-carga)
9. [Resultados](#resultados)
10. [Licença](#licença)

---

## Execução

Requisitos: Docker com **Compose 2.20 ou superior** (o Makefile verifica) e GNU make. Rust e PostgreSQL só são
necessários nos modos locais.

Antes de tudo, copie o modelo de ambiente do modo desejado e preencha **todas** as senhas e segredos (não há valores
padrão; o Compose falha se faltarem):

```bash
cp .env.docker.example .env.docker      # make up
cp .env.local.example  .env.local       # API local + banco em container
cp .env.external.example .env.external  # banco externo
openssl rand -base64 48                 # para gerar cada segredo
```

| API e worker | PostgreSQL | Comando |
|---|---|---|
| Container | Container do projeto | `make up` |
| Máquina local | Container do projeto | `make db-up`, `make dev-init`, `make dev-api` e `make dev-worker` |
| Container | Externo ou instalado na máquina | `make up-external` |
| Máquina local | Externo | `ENV_FILE=.env.external make dev-init dev-api` e `ENV_FILE=.env.external make dev-worker` |

A API e o worker não sabem onde o PostgreSQL está: a única diferença entre os modos é o arquivo de ambiente.

### `make up` (tudo em container)

Sobe os perfis `db` e `app`. Ordem garantida pelo Compose:
`postgres` saudável → `bootstrap` → `migrate` → `seed` → `api` + `worker`
(os temporários com `service_completed_successfully`; a dependência de `postgres` usa `required: false`,
para o perfil `app` funcionar sem o banco do projeto). Não exige Rust nem PostgreSQL na máquina.

```bash
make up
curl localhost:8080/health/ready
# Swagger UI (com API_DOCS=true, o padrão do .env.docker.example): http://localhost:8080/docs
make logs
make down       # mantém o volume
make reset      # apaga SOMENTE o volume do PostgreSQL do projeto (pede confirmação; recusado com .env.external)
```

O resultado é: banco `control`, owner (`SEED_OWNER_*`) e a primeira base (`SEED_TENANT_*`) provisionada.
O admin da primeira base é criado depois pelo owner:

```bash
TOKEN=$(curl -s -X POST localhost:8080/v1/platform/auth/login -H 'content-type: application/json' \
  -d '{"email":"owner@exemplo.com","password":"..."}' | jq -r .access_token)
TID=$(curl -s localhost:8080/v1/platform/tenants -H "authorization: Bearer $TOKEN" | jq -r '.items[0].id')
curl -X POST localhost:8080/v1/platform/tenants/$TID/admins -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"email":"admin@acme.com","name":"Admin","password":"senha-com-12+"}'
curl -X POST localhost:8080/v1/auth/login -H 'x-tenant-slug: acme' -H 'content-type: application/json' \
  -d '{"email":"admin@acme.com","password":"senha-com-12+"}'
```

### API local, banco em container

```bash
make db-up          # só o perfil db (não sobe a API); publica 127.0.0.1:${DB_PUBLISH_PORT:-5433}
make dev-init       # bootstrap + migrate + seed (cargo run --release)
make dev-api        # api serve em API_BIND (127.0.0.1:8080)
make dev-worker     # em outro terminal
```

### Banco externo

`make up-external` sobe só o perfil `app` (nunca o PostgreSQL do projeto). Use `DB_SSLMODE=verify-full` e coloque o
CA em `./certs/ca.crt`: o diretório é montado **somente leitura** em `/app/certs` e o `WORKDIR` da imagem é `/app`,
então `DB_SSL_ROOT_CERT=certs/ca.crt` funciona igual no container e na máquina local.

PostgreSQL instalado na própria máquina, com a API em container: use `DB_HOST=host.docker.internal` (o Compose define
`extra_hosts: ["host.docker.internal:host-gateway"]`). O PostgreSQL precisa aceitar conexões da rede Docker
(`listen_addresses` e uma linha em `pg_hba.conf` para a sub-rede do bridge, ex.: `172.16.0.0/12`).

Se vários projetos Compose compartilham a máquina, defina `COMPOSE_PROJECT_NAME` no arquivo de ambiente.

> Testado: um segundo PostgreSQL 18, fora do perfil `db`, com TLS e certificado próprio, `DB_SSLMODE=verify-full` e
> `DB_BOOTSTRAP=false`, nos dois modos (container e local).

### Comandos do binário

| Comando | O que faz | Credenciais |
|---|---|---|
| `api bootstrap` | Cria de forma idempotente `app_migrator`, `app_runtime` e o banco `control`. Nunca altera a senha de role existente. Com `DB_BOOTSTRAP=false`, só valida. | admin (ou migrator, ao validar) |
| `api migrate` | Migra o `control` e todos os tenants já provisionados (concorrência `MIGRATE_CONCURRENCY`), reaplica privilégios, sincroniza o catálogo. Um tenant com falha não para os outros; o comando retorna erro no fim. | migrator |
| `api seed` | Cria o owner e a primeira base. Idempotente, seguro em paralelo (advisory lock). | migrator |
| `api serve` | Inicia a API (não roda migrations). | runtime |
| `api worker` | Provisiona tenants pendentes. Acordado por `NOTIFY`, varre a cada 10 s. | migrator |
| `api healthcheck` | Consulta `/health/ready` da instância local (a imagem distroless não tem shell nem curl). | — |

Todos os comandos que acessam o banco repetem a conexão até `DB_CONNECT_RETRY_SECONDS`.

---

## Documentação da API (Swagger)

Com `API_DOCS=true`, a API serve:

| Caminho | Conteúdo |
|---|---|
| `/docs` | Swagger UI, para navegar e testar os endpoints |
| `/openapi.json` | Especificação OpenAPI 3.1, para gerar clientes ou importar no Postman, Insomnia ou Bruno |

O padrão é `false`: em produção nada disso é exposto e as rotas respondem 404. Os modelos `.env.docker.example` e
`.env.local.example` ligam a opção; o `.env.external.example` a deixa desligada.

**Testar pelo Swagger UI:**

1. Abra `http://localhost:8080/docs`.
2. Faça login:
   - usuário de tenant: `POST /v1/auth/login`, informando o slug do tenant no campo `X-Tenant-Slug`;
   - owner ou superusuário: `POST /v1/platform/auth/login`.
3. Copie o `access_token` da resposta.
4. Clique em **Authorize** e cole o token no esquema correspondente (`tenant_token` ou `platform_token`).
5. Com token de plataforma, as rotas de tenant (`/v1/users`, `/v1/profiles` etc.) também precisam do campo
   `X-Tenant-Slug`.

**Como a documentação é gerada (`src/openapi.rs`):**

- Cada handler declara caminho, parâmetros, corpo e respostas específicas com `#[utoipa::path]`. Os tipos de
  requisição e resposta derivam `ToSchema`, e os comentários `///` viram as descrições.
- A regra de acesso **não é escrita à mão**. Ela é aplicada a partir das mesmas `RouteDef` que montam o roteador e
  o middleware `enforce`:
  - esquema de segurança exigido;
  - feature necessária, descrita no texto da rota;
  - header `X-Tenant-Slug` nas rotas de tenant;
  - respostas de erro comuns: 401, 403, 404, 422, 503 etc., no formato `application/problem+json`.
- Testes unitários falham se:
  - uma rota do roteador não estiver documentada, ou se a documentação tiver uma rota que não existe;
  - a segurança de alguma rota divergir do `Access` declarado;
  - um parâmetro de caminho estiver faltando;
  - houver `operationId` repetido.
- Desempenho: a especificação é montada uma vez, na inicialização. O Swagger UI fica fora dos middlewares de
  autorização e de métricas, e os arquivos estáticos já vêm embutidos no binário (feature `vendored`; o build não
  baixa nada). As rotas da API não passam por nenhum código novo.

---

## Variáveis de ambiente

Não existe `DATABASE_URL`: as conexões são montadas por banco (`control` ou `tenant_*`) e por role.

| Variável | Uso | Padrão |
|---|---|---|
| `DB_HOST`, `DB_PORT`, `DB_CONTROL_NAME` | servidor e nome do banco control | — / 5432 / `control` |
| `DB_RUNTIME_USER`, `DB_RUNTIME_PASSWORD` | role da API | `app_runtime` / — |
| `DB_MIGRATOR_USER`, `DB_MIGRATOR_PASSWORD` | role de migrate/seed/worker | `app_migrator` / — |
| `DB_ADMIN_USER`, `DB_ADMIN_PASSWORD`, `DB_ADMIN_DATABASE` | só o bootstrap | — / — / `postgres` |
| `DB_BOOTSTRAP` | `false` = só valida (banco gerenciado) | `true` |
| `DB_SSLMODE`, `DB_SSL_ROOT_CERT` | TLS (`disable` … `verify-full`) | `prefer` / — |
| `DB_CONNECT_RETRY_SECONDS` | espera pelo banco | 30 (60 no Compose) |
| `API_BIND` | endereço da API (`0.0.0.0:8080` em container, `127.0.0.1:8080` local) | `127.0.0.1:8080` |
| `API_PORT` | porta publicada no host pelo Compose | 8080 |
| `METRICS_BIND`, `WORKER_METRICS_BIND` | `/metrics` em porta interna (não publicada por padrão) | `127.0.0.1:9090` / `:9091` |
| `API_DOCS` | serve Swagger UI em `/docs` e a especificação em `/openapi.json` | `false` |
| `JWT_PLATFORM_SECRET`, `JWT_TENANT_SECRET` | segredos distintos, ≥ 32 bytes | — |
| `JWT_ISSUER` | `iss` dos tokens | `simple-core` |
| `API_INSTANCES`, `MAX_OPEN_TENANT_POOLS`, `TENANT_POOL_MAX_CONNECTIONS`, `CONTROL_POOL_MAX_CONNECTIONS`, `PG_CONNECTION_RESERVE` | orçamento de conexões (abaixo) | 1 / 50 / 10 / 10 / 10 |
| `ACCESS_CACHE_CAPACITY`, `PLATFORM_CACHE_CAPACITY`, `TENANT_REGISTRY_CAPACITY` | capacidade dos caches | 200000 / 10000 / 10000 |
| `PASSWORD_HASH_CONCURRENCY` | vagas do semáforo do Argon2id | nº de núcleos |
| `LOGIN_RATE_*`, `LOGIN_RATE_WINDOW_SECONDS` | limites de login (plataforma precisa ser mais restritiva) | tenant 60/IP e 10/e-mail, plataforma 20 e 5, por 60 s |
| `MIGRATE_CONCURRENCY` | tenants migrados em paralelo | 4 |
| `SEED_OWNER_EMAIL`, `SEED_OWNER_NAME`, `SEED_OWNER_PASSWORD` (≥ 16), `SEED_TENANT_SLUG`, `SEED_TENANT_NAME` | seed, sem padrão | — |
| `POSTGRES_MAX_CONNECTIONS`, `POSTGRES_SHARED_BUFFERS`, `DB_PUBLISH_PORT` | container do PostgreSQL (o padrão de 1150 cobre 110 pools × 10 conexões) | 1150 / 1GB / 5433 |
| `API_CPUS`, `API_MEMORY`, `WORKER_CPUS`, `WORKER_MEMORY` | limites dos containers | 4 / 1g / 1 / 256m |

**Orçamento de conexões.** A API não inicia se
`API_INSTANCES × (MAX_OPEN_TENANT_POOLS × TENANT_POOL_MAX_CONNECTIONS + CONTROL_POOL_MAX_CONNECTIONS)`
passar de `max_connections − PG_CONNECTION_RESERVE`. O `PgListener` usa uma das conexões do `control`.

Cada serviço do Compose recebe somente as credenciais que usa: só o `bootstrap` vê as administrativas, e a `api` só
as de `app_runtime`. As credenciais do migrator nunca estão no container da API.

---

## Banco gerenciado (DB_BOOTSTRAP=false)

Quando o DBA cria os recursos, o `bootstrap` só valida (com as credenciais do migrator). Privilégios necessários:

```sql
CREATE ROLE app_migrator LOGIN CREATEDB NOSUPERUSER NOCREATEROLE PASSWORD '...';
CREATE ROLE app_runtime  LOGIN NOCREATEDB NOSUPERUSER NOCREATEROLE PASSWORD '...';
CREATE DATABASE control OWNER app_migrator TEMPLATE template0 ENCODING 'UTF8';
REVOKE ALL ON DATABASE control FROM PUBLIC;
GRANT CONNECT ON DATABASE control TO app_migrator, app_runtime;
```

- `app_migrator` precisa de `CREATEDB` (cria um banco por tenant e é dono dos bancos e objetos). Não precisa ser
  superusuário nem ter `CREATEROLE`.
- `app_runtime` precisa só de `LOGIN`. Todos os demais privilégios (por tabela e por coluna) são concedidos pelo
  `migrate` e pelo provisionamento.
- PostgreSQL 15 ou superior (o schema `public` pertence ao dono do banco).

A validação falha se `app_runtime` tiver `CREATEDB` ou se alguma das duas for superusuária.

---

## Migrar do banco em container para um banco externo

1. Pare a aplicação: `make down` (o volume do PostgreSQL permanece).
2. Suba só o banco do projeto (`make db-up`) e faça o dump **de todos os bancos da aplicação** (`control` e cada
   `tenant_*`), incluindo roles:
   ```bash
   docker compose --env-file .env.docker exec -T postgres pg_dumpall -U "$DB_ADMIN_USER" --roles-only > roles.sql
   for db in control $(docker compose --env-file .env.docker exec -T postgres psql -U "$DB_ADMIN_USER" -Atc \
       "select datname from pg_database where datname like 'tenant_%'"); do
     docker compose --env-file .env.docker exec -T postgres pg_dump -U "$DB_ADMIN_USER" -Fc "$db" > "$db.dump"
   done
   ```
3. No destino: crie as roles (com o `roles.sql`, ou pelo DBA conforme a seção anterior), crie cada banco com
   `OWNER app_migrator TEMPLATE template0 ENCODING 'UTF8'` e restaure com `pg_restore --no-owner --role=app_migrator -d <banco>`.
4. Reaplique as restrições de `CONNECT` e os privilégios: rode o `bootstrap` (ou valide com `DB_BOOTSTRAP=false`) e
   depois o `migrate`, que reaplica os grants do `control` e de todos os tenants. Para os bancos de tenant, execute
   também `REVOKE ALL ON DATABASE tenant_x FROM PUBLIC; GRANT CONNECT ON DATABASE tenant_x TO app_runtime;`.
5. Copie o `.env.docker` para `.env.external`, ajuste `DB_HOST`, `DB_PORT`, `DB_SSLMODE`, o CA em `./certs` e
   `MAX_OPEN_TENANT_POOLS` conforme o `max_connections` do destino, e suba com `make up-external`.

Os nomes `tenant_<uuid>` estão gravados em `tenants.db_name` e precisam ser mantidos.

---

## Arquitetura e decisões técnicas

```
src/main.rs              comandos do binário
src/app.rs               roteador, middlewares globais, /metrics, serve
src/config.rs            configuração só por ambiente; Secret nunca aparece em Debug
src/state.rs             AppState, invalidação de cache e PgListener
src/error.rs             RFC 9457 (application/problem+json)
src/openapi.rs           especificação OpenAPI (utoipa) e Swagger UI
src/password.rs          Argon2id em spawn_blocking + Semaphore
src/auth/                jwt, login/refresh/logout, middleware de acesso, rate limit
src/tenancy/             registro de tenants, pools por tenant, resolução do tenant, provisionamento
src/access/              catálogo, perfis, exceções, resolução de permissões e caches
src/users/               CRUD de usuários
src/platform/            endpoints de plataforma
src/cli/                 bootstrap, migrate, seed, worker, healthcheck
migrations/control/      schema do control (+ triggers de NOTIFY)
migrations/tenant/       schema de cada tenant
```

### Multi-tenancy

- Banco `control` com tenants, owner/superusuários, sessões e auditoria de plataforma; um banco
  `tenant_<uuid sem hífens>` por tenant, sem `tenant_id` nas tabelas.
- **Não há pool padrão nem fallback.** Todo acesso a dados de tenant passa por `TenantCtx`, criado só pelo middleware
  depois de resolver o tenant; os repositórios recebem explicitamente o executor desse pool.
- Token de tenant: o tenant vem só da claim assinada `tid`; `X-Tenant-Slug` divergente → 403. Token de plataforma:
  exige `X-Tenant-Slug`. Apenas tenants `active` são atendidos; suspenso → 403 para todos (inclusive no login).
- Registro de tenants em `moka` (TTL 30 s), por id e por slug, com cache negativo. Invalidação por trigger
  (`AFTER INSERT OR UPDATE ON tenants` → `pg_notify`), entregue só no `COMMIT`.
- Pools por tenant: `PgPool` lazy em `moka` com `EvictionPolicy::lru()`, `try_get_with`, `time_to_idle` de 10 min,
  `min_connections=0`, `idle_timeout` de 60 s, `acquire_timeout` de 1 s (→ 503) e `statement_timeout` de 2 s enviado
  no pacote de startup (sem ida extra ao banco).
- **Remoção de pool sem derrubar requisições:** o pool fica num `Arc<TenantPool>`; o `Drop` do último `Arc` fecha o
  pool numa task separada. Requisições que já pegaram o handle terminam normalmente, e o cache libera a vaga na hora.

### Roles do PostgreSQL

- `app_migrator` (dono de tudo, `CREATEDB`) usado só por `migrate`, `seed`, `worker` e bootstrap.
- `app_runtime` (API) no `control`: `tenants` SELECT/INSERT e UPDATE só em `name, status, last_error, updated_at`;
  `platform_users` SELECT/INSERT/DELETE e UPDATE só em `name, password_hash, is_active, updated_at`;
  `platform_refresh_tokens` DML; `platform_audit` SELECT/INSERT.
- Nos tenants: DML nas tabelas, `access_audit` só SELECT/INSERT, e `modules`/`features` só SELECT (o catálogo é
  somente leitura na API). `REVOKE ALL ON DATABASE ... FROM PUBLIC` (inclui `TEMP`) e `CONNECT` só para `app_runtime`.
- O bootstrap envia **verificadores SCRAM-SHA-256** calculados no cliente (`PASSWORD 'SCRAM-SHA-256$...'`): a senha
  em texto nunca aparece em SQL nem em log, nem do lado do servidor.

### Autorização (negada por padrão)

Toda rota é declarada como `RouteDef { method, path, access, handler }`, com `Access::{Public, Platform,
PlatformOwner, TenantToken, Feature(f), SelfOr(f)}`, e o roteador aplica o middleware `enforce` a cada uma. Os
handlers pegam o contexto por extractor; sem o middleware, o extractor falha com 500 (nunca libera). Um teste percorre
todas as rotas e verifica 401 sem token, 403 sem feature, 403 para superusuário em rota de owner e 403 para token de
tenant em rota de plataforma.

- Permissões efetivas resolvidas numa única ida ao banco (usuário + a query de features como subquery `ARRAY(...)`).
- `Arc<Principal>` imutável por `(tenant_id, user_id)` com `is_active`, `is_tenant_admin` e um **bitset `u64`** de
  features: verificação O(1) sem hash nem alocação.
- Cache de plataforma separado (`role`, `is_active`). **Rotas do owner consultam `role`/`is_active` direto no
  `control`, sem cache.**
- TTL 30 s, capacidade configurável, coalescência com `try_get_with`. Alteração de usuário invalida a entrada; de perfil,
  o tenant inteiro (`invalidate_entries_if`).
- Depois do commit: invalida localmente e publica `pg_notify('cache_invalidation', ...)` no `control` (o NOTIFY é por
  banco). Falha na publicação → log + métrica `cache_invalidation_publish_failures_total`; o TTL limita a defasagem.
- Um único `PgListener` por instância, com `eager_reconnect`; ao reconectar, limpa todos os caches.

### Proteções contra escalada

- Owner único (índice parcial único) e criado só pelo seed; nenhuma rota tem campo `role`, e todas as queries de
  superusuário filtram `role = 'superuser'`.
- `is_tenant_admin` só muda pelos endpoints do owner; campos desconhecidos são rejeitados (422).
- Quem não tem acesso total só concede features que possui — via perfil (só o que for **adicionado** é checado),
  exceção `allow` e também remoção de `deny` (que equivale a conceder).
- Usuário comum não altera os próprios perfis/exceções; admin de tenant só é alterado/excluído por usuário de plataforma
  (o próprio admin pode trocar nome e senha).
- Último admin ativo: o `UPDATE`/`DELETE` acontece primeiro e, se o alvo era admin ativo, a transação pega
  `pg_advisory_xact_lock` e verifica se ainda resta algum. Em READ COMMITTED isso serializa as corridas corretamente
  (testado com remoções concorrentes).

### Autenticação

- JWT HS256 de 15 min, algoritmo fixo, `exp`/`iss`/`aud`/`sub` obrigatórios, segredos e audiências diferentes para
  plataforma (`simple-core:platform`) e tenant (`simple-core:tenant`). Sem papel nem permissões no token.
- Refresh tokens opacos de 256 bits, guardados como SHA-256, 30 dias, rotacionados a cada uso; reuso de token revogado
  revoga todas as sessões do usuário. Troca de senha e desativação também revogam as sessões.
- Logins: mesmo 401 para tenant inexistente, e-mail inexistente, senha errada e conta inativa, sempre executando
  Argon2id (hash fictício quando não há usuário).
- Rate limit de login por escopo (plataforma ou tenant), IP e e-mail, em janela fixa (`moka` com TTL) → 429.

### Desempenho e resiliência

- Argon2id `m=19456, t=2, p=1` em `spawn_blocking`, `Semaphore` do tamanho do nº de núcleos, espera máx. 1 s → 503.
- Sem lock global no caminho da requisição; com caches quentes, resolução do tenant e autorização não consultam o banco.
- Logs JSON via `tracing-appender` não bloqueante, sem log por requisição em `info`.
- Métricas: latência por rota (`route` = padrão da rota, nunca o tenant), pools abertos, conexões abertas/em uso, fila
  do hash, fila de provisionamento, acertos/erros de cada cache, invalidações, timeouts e panics.
- Timeout global de 3 s (503), body máx. 16 KB, `CatchPanicLayer` (com `panic="unwind"` o processo não cai), graceful
  shutdown em SIGTERM.
- Release com `opt-level=3`, `lto="fat"`, `codegen-units=1`, `strip=true`, `panic="unwind"`.
- Paginação por keyset (`WHERE id > $1 ORDER BY id LIMIT $2`), colunas explícitas, `INSERT` direto com 23505 → 409,
  `UPDATE ... RETURNING` numa query, `rows_affected` no `DELETE`, UUIDv7 gerado na aplicação.

### Provisionamento

`seed` e `worker` usam a mesma rotina retomável: advisory lock por tenant em conexão dedicada → revalida `db_name` →
`CREATE DATABASE ... TEMPLATE template0 ENCODING 'UTF8'` se não existir (identificador escapado) → `CONNECT` e
privilégios → migrations do tenant → catálogo (upsert por `code`, numa transação) → `active`. Em erro: `failed` com
`last_error` curto e sem segredos; `POST /v1/platform/tenants/{id}/retry` volta para `provisioning`. O worker reivindica
com `FOR UPDATE SKIP LOCKED` e é acordado pelo trigger que publica em `tenant_provisioning`.

As migrations são incorporadas ao binário (`sqlx::migrate!`) e devem seguir expand/contract; o catálogo nunca remove
features.

### SQLx com dois schemas

`make sqlx-prepare` cria o banco temporário `sqlx_check`, aplica `migrations/control` e `migrations/tenant` (não há
tabelas em comum), roda `cargo sqlx prepare` e remove o banco. `make sqlx-check` falha se `.sqlx` estiver desatualizado.
A imagem compila com `SQLX_OFFLINE=true`, sem banco.

---

## Testes

```bash
make db-up
make test          # ou: cargo test (lê .env.local se DB_HOST não estiver definido)
make fmt lint sqlx-check
```

Cada teste de integração cria o próprio `control` (`test_<uuid>`) e seus tenants num PostgreSQL real, então os testes
rodam em paralelo; ao final os bancos são removidos. Cobertura (43 testes: 13 unitários e 30 de integração):

- **Plataforma** (`tests/platform.rs`): só o owner cria tenants/superusuários/admins (superusuário, admin e usuário
  comum → 403); superusuário entra em todos os tenants mas não altera `is_tenant_admin`; ninguém cria, altera ou exclui
  o owner, inclusive em requisições concorrentes e por SQL com `app_runtime`; superusuário desativado perde o acesso
  nas duas instâncias; escopos de token; varredura de todas as rotas; rate limit; `me` e superusuários.
- **Isolamento** (`tests/isolation.rs`): mesmo e-mail em dois tenants e na plataforma; token do tenant A sem acesso a
  B (inclusive com outro `X-Tenant-Slug` e refresh); admin restrito ao próprio tenant; suspensão rápida e reativação
  nas duas instâncias; `app_runtime` sem DDL, `CREATE DATABASE`, alteração de `db_name`/`role`/auditorias e sem `TEMP`;
  ACL do banco sem `PUBLIC`.
- **Acesso** (`tests/access.rs`): CRUD com 404/409/422/413 e e-mails duplicados concorrentes; paginação keyset;
  permissões por perfil, soma de perfis e `allow`; `deny` vencendo e exceções expiradas ignoradas; inativo/401/403;
  autoatendimento de nome e senha; invalidação entre duas instâncias afetando só o tenant certo; proteções contra
  escalada; corrida no último admin; rotação e reuso de refresh token; catálogo, perfis e auditoria.
- **Operação** (`tests/operation.rs`): seed executado duas vezes e quatro em paralelo (um owner, uma base, senha e OID
  do banco inalterados; e-mail de superusuário não é promovido); dois workers disputando o mesmo tenant; provisionamento
  interrompido em cada uma das 5 etapas e retomado; `migrate` com um tenant falhando sem afetar os outros e retornando
  erro; remoção de pool do cache com consulta em andamento e LRU com capacidade 2; bootstrap idempotente e modo de
  validação.
- **Documentação** (`tests/docs.rs` e `src/openapi.rs`): `/docs` e `/openapi.json` só existem com `API_DOCS=true` e
  não liberam as rotas da API; a especificação cobre exatamente as rotas do roteador, com segurança e parâmetros
  coerentes com o `Access` de cada uma.

---

## Testes de carga

```bash
make up                                                     # stack com a imagem de release
docker compose --env-file .env.docker -f compose.yaml -f load/compose.load.yaml \
  --profile db --profile app up -d --wait                   # aplica o pinning de CPU da carga
make load-populate                                          # 100 tenants, 1M+99k usuários, perfis, exceções
make load                                                   # todos os cenários (≈ 40 min)
make load-scenario S=get_by_id RATE=15000                   # um cenário
```

- **População** (`load/populate.sh`): 100 tenants criados pela API e provisionados pelo worker; `load-000` com
  1.000.000 de usuários e os outros 99 com 1.000; 20 perfis por tenant; 1 ou 2 perfis por usuário; exceções em 5% dos
  usuários; senha real Argon2id. Tokens para 5.950 usuários diferentes (1.000 no tenant grande + 50 em cada pequeno),
  assinados pelo k6 com o segredo de tenant.
- **Isolamento de CPU** (`load/compose.load.yaml`): API em `cpuset 0-3`, PostgreSQL em `4-7`, k6 em `8-15` — núcleos
  físicos separados (SMT: 0-1, 2-3, ...). `fsync` e `synchronous_commit` ligados.
- **Método:** taxa de chegada constante (k6 `constant-arrival-rate`), 30 s de aquecimento e 3 min de medição; o resumo
  considera só a medição. Se a API não acompanha, o RPS medido fica abaixo da taxa pedida. CPU/memória por
  `docker stats`, conexões por `pg_stat_activity`, acerto dos caches pela diferença dos contadores do `/metrics`.
- **Mistura**: 80% `GET` por id, 15% listagem, 4% `PATCH` de nome e 1% `DELETE` (vítimas criadas antes de cada
  execução, removidas uma única vez). Em "todos os tenants", tráfego Zipf (o tenant *i* recebe peso 1/(i+1)).
- Apenas no override de carga os limites de login são altos: o cenário de login mede o Argon2id, não o rate limit.

### Hardware

| Item | Valor |
|---|---|
| CPU | AMD Ryzen 7 9800X3D (8 núcleos / 16 threads) |
| RAM | 32 GB (VM do Docker: 16 GB, 16 vCPUs) |
| SO | Windows 11 Pro + Docker Desktop 29.5 (WSL2) |
| API | 4 vCPUs = 2 núcleos físicos com SMT (`cpuset 0-3`), limite 2 GB |
| PostgreSQL 18 | 4 vCPUs = 2 núcleos físicos (`cpuset 4-7`), `shared_buffers=1GB`, `max_connections=1150` |
| k6 | 4 núcleos físicos (`cpuset 8-15`), na mesma rede bridge |

Observação: "4 vCPUs" aqui são 2 núcleos físicos com SMT, e o Windows continua agendando trabalho do host nos mesmos
núcleos da VM. Repetições do mesmo cenário variaram bastante (ver abaixo), principalmente no p99.

## Resultados

Configuração final: imagem release (`lto="fat"`, mimalloc), `TENANT_POOL_MAX_CONNECTIONS=10`,
`MAX_OPEN_TENANT_POOLS=110`, `CONTROL_POOL_MAX_CONNECTIONS=10`, PostgreSQL com `max_connections=1150`,
`fsync`/`synchronous_commit` ligados. Cada linha: 30 s de aquecimento + 3 min de medição, taxa de chegada constante.
Dados brutos em `load/results/30-final/` (JSON do k6, amostras de CPU/memória/conexões e contadores dos caches).

| Cenário | Taxa pedida | RPS | p50 ms | p95 ms | p99 ms | Erros | CPU API méd/máx | Mem API máx | CPU PG méd/máx | Mem PG máx | Conexões runtime méd/máx | Acerto dos caches |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| get_by_id | 15000 | 14994 | 0.40 | 0.97 | 7.25 | 0.000% | 282% / 299% | 69 MiB | 178% / 191% | 482 MiB | 12 / 12 (ativas máx 3) | tenant_pool 100.00% tenant_registry 100.00% access 99.78% |
| list | 4000 | 4000 | 0.49 | 0.64 | 0.75 | 0.000% | 98% / 112% | 72 MiB | 71% / 81% | 605 MiB | 12 / 12 (ativas máx 2) | tenant_pool 100.00% tenant_registry 100.00% access 99.17% |
| mix_single | 9000 | 8989 | 0.44 | 2.43 | 14.01 | 0.000% | 211% / 235% | 72 MiB | 142% / 161% | 710 MiB | 20 / 20 (ativas máx 5) | tenant_pool 100.00% tenant_registry 100.00% access 99.63% |
| mix_all | 9000 | 9000 | 0.46 | 2.08 | 3.07 | 0.000% | 213% / 225% | 103 MiB | 161% / 189% | 2862 MiB | 688 / 691 (ativas máx 6) | tenant_pool 100.00% tenant_registry 99.96% access 97.72% |
| pool_churn | 8000 | 947 | 765.89 | 1532.24 | 1592.33 | 0.060% | 191% / 201% | 157 MiB | 382% / 389% | 1408 MiB | 137 / 237 (ativas máx 3) | tenant_pool 41.13% tenant_registry 99.65% access 84.39% |
| noisy_neighbor (normais) | 1500 | 1500 | 0.44 | 0.76 | 1.18 | 0.000% | 163% / 196% | 115 MiB | 98% / 120% | 2715 MiB | 724 / 724 (ativas máx 1) | tenant_pool 100.00% tenant_registry 99.96% access 97.88% |
| noisy_neighbor (barulhento, 6000/s) | 1500 | 6000 | 0.40 | 0.57 | 0.95 | 0.000% | 163% / 196% | 115 MiB | 98% / 120% | 2715 MiB | 724 / 724 (ativas máx 1) | tenant_pool 100.00% tenant_registry 99.96% access 97.88% |
| login | 150 | 150 | 11.15 | 13.73 | 16.02 | 0.000% | 160% / 166% | 186 MiB | 10% / 21% | 2740 MiB | 238 / 729 (ativas máx 1) | tenant_pool 100.00% tenant_registry 99.98% |
| create | 150 | 150 | 12.28 | 34.99 | 389.73 | 0.326% | 164% / 182% | 190 MiB | 10% / 13% | 950 MiB | 20 / 20 (ativas máx 10) | tenant_pool 100.00% tenant_registry 99.98% access 81.22% |
| read_with_invalidations | 10000 | 9999 | 0.43 | 0.80 | 2.47 | 0.000% | 224% / 241% | 111 MiB | 149% / 157% | 954 MiB | 20 / 20 (ativas máx 3) | tenant_pool 100.00% tenant_registry 100.00% platform 98.34% access 80.17% |
| read_with_provisioning | 10000 | 10000 | 0.39 | 0.58 | 1.48 | 0.000% | 214% / 228% | 115 MiB | 129% / 144% | 1154 MiB | 20 / 20 (ativas máx 2) | tenant_pool 100.00% tenant_registry 100.00% access 99.67% |

CPU em % de um vCPU (400% = os 4 vCPUs). "Conexões" = conexões de `app_runtime` em `pg_stat_activity`.
"Acerto dos caches" = hits/(hits+misses) durante a medição, por cache.

### Metas

| Meta | Resultado | |
|---|---|---|
| `GET` por id, um tenant, caches quentes: ≥ 15.000 RPS, p99 < 10 ms, sem erros | 14.994 RPS (taxa pedida 15.000, sem descartes relevantes), p99 7,25 ms, 0 erros | atingida |
| Mistura em um tenant: ≥ 8.000 RPS, p99 < 25 ms, erros < 0,1% | 8.989 RPS (pedidos 9.000), p99 14,0 ms, 0 erros | atingida |

O teste usa taxa constante: o RPS acompanha a taxa pedida e não mede a capacidade máxima acima dela. No `get_by_id` a
API ficou em ~280% de CPU (de 400%), então há alguma folga, mas não foi medida.

### Histórico de otimizações (uma alteração por vez)

| # | Alteração | Cenário | Antes | Depois | Decisão |
|---|---|---|---|---|---|
| 01 | Linha de base (pool 10, `test_before_acquire` padrão) | `get_by_id` 15k/s | — | 12.926 RPS, p99 115 ms; repetição: 14.735 RPS, p99 68 ms | — |
| 02 | `test_before_acquire(false)`: o sqlx fazia um ping no PostgreSQL a cada `acquire` | `get_by_id` 15k/s | p95 50,7 ms, p99 68,1 ms, CPU API 329%, PG 220% | 14.854 RPS, p95 3,0 ms, p99 29,6 ms, CPU API 290%, PG 181% | mantida |
| 03 | mimalloc × alocador padrão (latência) | `get_by_id` ×2, `mix_single` | p99 29,6 / 14,3 ms; mix p99 3,18 ms | p99 19,8 / 27,8 ms; mix p99 3,14 ms | sem diferença além do ruído |
| 04 | Pools para todos os tenants ativos: 3 conexões × 120 pools (antes 10 × 25) | `mix_all` 9k/s | 1.873 RPS, p99 1.520 ms, **78% de erros** (portas efêmeras esgotadas pela troca de pools: `Cannot assign requested address`) | 8.985 RPS, p99 6,5 ms, 0% | causa confirmada, ver #07 |
| 05 | `net.ipv4.tcp_tw_reuse=1` no container da API | `pool_churn` | 1.832 RPS, **86,7% de erros** | 866–947 RPS, 0,06–0,4% de erros | mantida |
| 06 | mimalloc (memória) | `login` 150/s | RSS da API 1.493 MiB (fragmentação das arenas da glibc com os buffers de 19 MiB do Argon2id em várias threads de `spawn_blocking`) | 133 MiB, mesma latência | **mimalloc mantido** (padrão da feature) |
| 07 | 3 conexões por tenant eram poucas para o tenant quente | `get_by_id` | 3 conexões: 8.568 RPS, p50 92 ms (fila no pool, CPU da API 200%) | 6 conexões: p99 63 ms; 10 conexões: p99 10,7 ms | volta para 10 × 110 pools, `max_connections=1150` |

Também foi corrigido o gerador: assinar JWTs por VU e copiar os tenants em cada VU deixava o k6 em 388% de CPU e
4 GB de memória, limitando o `mix_all` a 4.322 RPS. Os tokens passaram a ser assinados uma vez, num `SharedArray`.

Lições: a amostragem de `pg_stat_activity` a cada 5 s (máx. 5 conexões ativas) subestimou a concorrência real no
pool; a decisão do tamanho do pool só ficou correta medindo latência com 3, 6 e 10 conexões. E há variância
considerável entre execuções idênticas nesta máquina (01 contra a repetição), principalmente no p99.

### Gargalos e observações

- **Leitura num tenant (`get_by_id`)**: CPU da API (~280–300% dos 400%) e do PostgreSQL (~180%). Com os caches quentes,
  a resolução do tenant e a autorização não vão ao banco (acerto de 99,8% no `access`, 100% em registro e pools);
  sobra uma query por PK por requisição.
- **Troca de pools (`pool_churn`, 20 pools para 100 tenants com tráfego uniforme)**: ~80% das requisições abrem uma
  conexão nova. O limite é o **PostgreSQL saturado (≈ 385% de CPU) abrindo conexões** (fork do backend + SCRAM), em
  ~950 RPS e p99 ~1,6 s. O `tcp_tw_reuse` eliminou os erros por portas esgotadas, mas a vazão continua limitada.
  **Meta não aplicável, mas o resultado é ruim por natureza**: na prática, `MAX_OPEN_TENANT_POOLS` precisa cobrir os
  tenants ativos (como no `mix_all`, que roda limpo); reduzir esse custo exigiria um pooler externo (PgBouncer, fora
  de escopo).
- **Mistura em todos os tenants (Zipf)**: 9.000 RPS, p99 3,1 ms, sem erros. O custo é o número de conexões abertas
  (~690 com 100 tenants ativos): os pools são lazy, mas conexões de tenants com tráfego ficam abertas até 60 s ociosas.
- **Tenant barulhento**: com um tenant a 6.000 req/s, os outros 98 tenants (1.500 req/s somados) ficaram com p99 de
  1,2 ms. Cada tenant tem o próprio pool, então um tenant quente não esgota as conexões dos demais.
- **Invalidações durante a leitura** (o tenant inteiro invalidado 2×/s): 9.999 RPS, p99 2,5 ms. O acerto do cache
  `access` cai para 80%, e a coalescência do `try_get_with` evita a avalanche de recargas.
- **Provisionamento durante a leitura** (um tenant novo a cada 10 s): 10.000 RPS, p99 1,5 ms, sem impacto visível.
- **Login e POST**: limitados intencionalmente pelo Argon2id (`m=19456,t=2,p=1`, ~11 ms por hash). A 150 req/s, o
  login ficou com p99 16 ms e 0 erros. O `POST /v1/users` teve p99 15 ms e 0 erros na bateria anterior (`20-final`),
  mas nesta teve p99 390 ms e 0,33% de erros (503 por espera > 1 s no semáforo do hash) com a mesma configuração do
  hash: sinal de interferência pontual de CPU (o host Windows divide os núcleos com a VM). Os parâmetros do Argon2id
  nunca foram reduzidos para melhorar o benchmark.
- **Memória**: API entre 70 e 190 MiB em todos os cenários (com mimalloc).

### Limitações do ambiente

- Docker Desktop no Windows (VM WSL2): o host continua usando os mesmos núcleos, o que aumenta a variância.
- "4 vCPUs" = 2 núcleos físicos com SMT, para a API e para o PostgreSQL.
- k6 na mesma máquina, em núcleos separados, falando com a API pela rede bridge do Docker.

---

## Licença

Licenciado, à sua escolha, sob qualquer uma das duas licenças:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

Salvo declaração explícita em contrário, qualquer contribuição enviada para inclusão neste projeto, conforme definido
na licença Apache-2.0, será licenciada da mesma forma dupla, sem termos ou condições adicionais.
