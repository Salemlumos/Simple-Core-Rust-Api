# simple-core

**English** | [Português](README.pt-BR.md)

Multi-tenant user management REST API with access control, built for low latency and high throughput.

- Stable Rust (1.99), Tokio, Axum 0.8, Tower
- PostgreSQL 18 with SQLx 0.9 (compile-time checked queries, offline mode with `.sqlx`)
- `moka` (caches), `argon2` (Argon2id), `jsonwebtoken` (HS256), `tracing` (non-blocking JSON), Prometheus metrics
- OpenAPI 3.1 generated from code (`utoipa`) with Swagger UI at `/docs`
- Docker Compose (`db` and `app` profiles) and k6 for load testing

A single binary, `api`, with the commands `bootstrap`, `migrate`, `seed`, `serve`, `worker` and `healthcheck`.

---

## Contents

1. [Running](#running)
2. [API documentation (Swagger)](#api-documentation-swagger)
3. [Environment variables](#environment-variables)
4. [Managed database (DB_BOOTSTRAP=false)](#managed-database-db_bootstrapfalse)
5. [Moving from the container database to an external one](#moving-from-the-container-database-to-an-external-one)
6. [Architecture and technical decisions](#architecture-and-technical-decisions)
7. [Tests](#tests)
8. [Load tests](#load-tests)
9. [Results](#results)
10. [Releases and versioning](#releases-and-versioning)
11. [Starting a new project from this base](#starting-a-new-project-from-this-base)
12. [License](#license)

---

## Running

Requirements: Docker with **Compose 2.20 or later** (the Makefile checks it) and GNU make. Rust and PostgreSQL are
only needed for the local modes.

First, copy the environment template for the mode you want and fill in **all** passwords and secrets (there are no
defaults; Compose fails if any is missing):

```bash
cp .env.docker.example .env.docker      # make up
cp .env.local.example  .env.local       # local API + database in a container
cp .env.external.example .env.external  # external database
openssl rand -base64 48                 # to generate each secret
```

| API and worker | PostgreSQL | Command |
|---|---|---|
| Container | Project container | `make up` |
| Local machine | Project container | `make db-up`, `make dev-init`, `make dev-api` and `make dev-worker` |
| Container | External or installed on the machine | `make up-external` |
| Local machine | External | `ENV_FILE=.env.external make dev-init dev-api` and `ENV_FILE=.env.external make dev-worker` |

The API and the worker don't know where PostgreSQL runs: the only difference between the modes is the environment file.

### `make up` (everything in containers)

Starts the `db` and `app` profiles. Order enforced by Compose:
`postgres` healthy → `bootstrap` → `migrate` → `seed` → `api` + `worker`
(the one-shot services use `service_completed_successfully`; the dependency on `postgres` uses `required: false`,
so the `app` profile works without the project database). Requires neither Rust nor PostgreSQL on the machine.

```bash
make up
curl localhost:8080/health/ready
# Swagger UI (with API_DOCS=true, the default in .env.docker.example): http://localhost:8080/docs
make logs
make down       # keeps the volume
make reset      # deletes ONLY the project's PostgreSQL volume (asks for confirmation; refused with .env.external)
```

The result: the `control` database, the owner (`SEED_OWNER_*`) and the first tenant (`SEED_TENANT_*`) provisioned.
The first tenant's admin is then created by the owner:

```bash
TOKEN=$(curl -s -X POST localhost:8080/v1/platform/auth/login -H 'content-type: application/json' \
  -d '{"email":"owner@example.com","password":"..."}' | jq -r .access_token)
TID=$(curl -s localhost:8080/v1/platform/tenants -H "authorization: Bearer $TOKEN" | jq -r '.items[0].id')
curl -X POST localhost:8080/v1/platform/tenants/$TID/admins -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' -d '{"email":"admin@acme.com","name":"Admin","password":"password-12+chars"}'
curl -X POST localhost:8080/v1/auth/login -H 'x-tenant-slug: acme' -H 'content-type: application/json' \
  -d '{"email":"admin@acme.com","password":"password-12+chars"}'
```

### Local API, database in a container

```bash
make db-up          # db profile only (no API); publishes 127.0.0.1:${DB_PUBLISH_PORT:-5433}
make dev-init       # bootstrap + migrate + seed (cargo run --release)
make dev-api        # api serve on API_BIND (127.0.0.1:8080)
make dev-worker     # in another terminal
```

### External database

`make up-external` starts only the `app` profile (never the project's PostgreSQL). Use `DB_SSLMODE=verify-full` and
put the CA in `./certs/ca.crt`: the directory is mounted **read-only** at `/app/certs` and the image's `WORKDIR` is
`/app`, so `DB_SSL_ROOT_CERT=certs/ca.crt` works the same in the container and on the local machine.

PostgreSQL installed on the host, with the API in a container: use `DB_HOST=host.docker.internal` (Compose sets
`extra_hosts: ["host.docker.internal:host-gateway"]`). PostgreSQL must accept connections from the Docker network
(`listen_addresses` and a `pg_hba.conf` line for the bridge subnet, e.g. `172.16.0.0/12`).

If several Compose projects share the machine, set `COMPOSE_PROJECT_NAME` in the environment file.

> Tested: a second PostgreSQL 18, outside the `db` profile, with TLS and its own certificate, `DB_SSLMODE=verify-full`
> and `DB_BOOTSTRAP=false`, in both modes (container and local).

### Binary commands

| Command | What it does | Credentials |
|---|---|---|
| `api bootstrap` | Idempotently creates `app_migrator`, `app_runtime` and the `control` database. Never changes the password of an existing role. With `DB_BOOTSTRAP=false`, it only validates. | admin (or migrator, when validating) |
| `api migrate` | Migrates `control` and every provisioned tenant (concurrency `MIGRATE_CONCURRENCY`), reapplies privileges and syncs the catalog. A failing tenant doesn't stop the others; the command returns an error at the end. | migrator |
| `api seed` | Creates the owner and the first tenant. Idempotent and safe to run in parallel (advisory lock). | migrator |
| `api serve` | Starts the API (does not run migrations). | runtime |
| `api worker` | Provisions pending tenants. Woken up by `NOTIFY`, also polls every 10 s. | migrator |
| `api healthcheck` | Calls `/health/ready` on the local instance (the distroless image has neither shell nor curl). | — |

Every command that touches the database retries the connection for up to `DB_CONNECT_RETRY_SECONDS`.

---

## API documentation (Swagger)

With `API_DOCS=true`, the API serves:

| Path | Content |
|---|---|
| `/docs` | Swagger UI, to browse and try the endpoints |
| `/openapi.json` | OpenAPI 3.1 specification, to generate clients or import into Postman, Insomnia or Bruno |

The default is `false`: in production none of this is exposed and these paths return 404. The
`.env.docker.example` and `.env.local.example` templates turn it on; `.env.external.example` leaves it off.

To get the specification without running the API or a database (e.g. to generate a client), use
`make openapi` (writes `openapi.json`; change the path with `OPENAPI_OUT=...`) or `api openapi > openapi.json`.

**Trying it in Swagger UI:**

1. Open `http://localhost:8080/docs`.
2. Log in:
   - tenant user: `POST /v1/auth/login`, filling in the tenant slug in the `X-Tenant-Slug` field;
   - owner or superuser: `POST /v1/platform/auth/login`.
3. Copy the `access_token` from the response.
4. Click **Authorize** and paste the token into the matching scheme (`tenant_token` or `platform_token`).
5. With a platform token, tenant routes (`/v1/users`, `/v1/profiles`, etc.) also need the `X-Tenant-Slug` field.

**How the documentation is generated (`src/openapi.rs`):**

- Each handler declares its path, parameters, body and specific responses with `#[utoipa::path]`. Request and
  response types derive `ToSchema`, and `///` comments become the descriptions.
- The access rule is **not written by hand**. It is applied from the same `RouteDef`s that build the router and the
  `enforce` middleware:
  - required security scheme;
  - required feature, described in the route text;
  - `X-Tenant-Slug` header on tenant routes;
  - common error responses: 401, 403, 404, 422, 503, etc., as `application/problem+json`.
- Unit tests fail if:
  - a router route is not documented, or the documentation has a route that doesn't exist;
  - the security of any route differs from its declared `Access`;
  - a path parameter is missing;
  - an `operationId` is duplicated.
- Performance: the specification is built once, at startup. Swagger UI sits outside the authorization and metrics
  middlewares, and its static files are embedded in the binary (`vendored` feature; the build downloads nothing).
  API routes don't run any new code.

**Errors:** every error is `application/problem+json` (RFC 9457) with a stable `code` field. It is either the default
for the status (`unauthorized`, `not_found`, `validation_failed`, `rate_limited`, `unavailable`...) or a specific 403/409
reason (`missing_feature`, `cannot_grant`, `email_taken`, `slug_taken`, `last_tenant_admin`...; full list in
`src/error.rs`). In a 422, each item of `errors` also has `field` and `code` (`required`, `invalid_email`,
`invalid_length`...). Clients should branch and translate on `code`; `detail` and `message` are English text for
humans and may change.

---

## Environment variables

There is no `DATABASE_URL`: connections are built per database (`control` or `tenant_*`) and per role.

| Variable | Purpose | Default |
|---|---|---|
| `DB_HOST`, `DB_PORT`, `DB_CONTROL_NAME` | server and name of the control database | — / 5432 / `control` |
| `DB_RUNTIME_USER`, `DB_RUNTIME_PASSWORD` | API role | `app_runtime` / — |
| `DB_MIGRATOR_USER`, `DB_MIGRATOR_PASSWORD` | migrate/seed/worker role | `app_migrator` / — |
| `DB_ADMIN_USER`, `DB_ADMIN_PASSWORD`, `DB_ADMIN_DATABASE` | bootstrap only | — / — / `postgres` |
| `DB_BOOTSTRAP` | `false` = validate only (managed database) | `true` |
| `DB_SSLMODE`, `DB_SSL_ROOT_CERT` | TLS (`disable` … `verify-full`) | `prefer` / — |
| `DB_CONNECT_RETRY_SECONDS` | how long to wait for the database | 30 (60 in Compose) |
| `API_BIND` | API address (`0.0.0.0:8080` in a container, `127.0.0.1:8080` locally) | `127.0.0.1:8080` |
| `API_PORT` | port published on the host by Compose | 8080 |
| `METRICS_BIND`, `WORKER_METRICS_BIND` | `/metrics` on an internal port (not published by default) | `127.0.0.1:9090` / `:9091` |
| `API_DOCS` | serves Swagger UI at `/docs` and the specification at `/openapi.json` | `false` |
| `AUTH_COOKIE_SECURE` | `Secure` attribute of the refresh cookie (turn off only for plain HTTP outside `localhost`) | `true` |
| `JWT_PLATFORM_SECRET`, `JWT_TENANT_SECRET` | distinct secrets, ≥ 32 bytes | — |
| `JWT_ISSUER` | token `iss` | `simple-core` |
| `API_INSTANCES`, `MAX_OPEN_TENANT_POOLS`, `TENANT_POOL_MAX_CONNECTIONS`, `CONTROL_POOL_MAX_CONNECTIONS`, `PG_CONNECTION_RESERVE` | connection budget (below) | 1 / 50 / 10 / 10 / 10 |
| `ACCESS_CACHE_CAPACITY`, `PLATFORM_CACHE_CAPACITY`, `TENANT_REGISTRY_CAPACITY` | cache capacities | 200000 / 10000 / 10000 |
| `PASSWORD_HASH_CONCURRENCY` | Argon2id semaphore permits | number of cores |
| `LOGIN_RATE_*`, `LOGIN_RATE_WINDOW_SECONDS` | login limits (platform must be stricter) | tenant 60/IP and 10/e-mail, platform 20 and 5, per 60 s |
| `MIGRATE_CONCURRENCY` | tenants migrated in parallel | 4 |
| `SEED_OWNER_EMAIL`, `SEED_OWNER_NAME`, `SEED_OWNER_PASSWORD` (≥ 16), `SEED_TENANT_SLUG`, `SEED_TENANT_NAME` | seed, no defaults | — |
| `POSTGRES_MAX_CONNECTIONS`, `POSTGRES_SHARED_BUFFERS`, `DB_PUBLISH_PORT` | PostgreSQL container (the default of 1150 covers 110 pools × 10 connections) | 1150 / 1GB / 5433 |
| `API_CPUS`, `API_MEMORY`, `WORKER_CPUS`, `WORKER_MEMORY` | container limits | 4 / 1g / 1 / 256m |

**Connection budget.** The API refuses to start if
`API_INSTANCES × (MAX_OPEN_TENANT_POOLS × TENANT_POOL_MAX_CONNECTIONS + CONTROL_POOL_MAX_CONNECTIONS)`
exceeds `max_connections − PG_CONNECTION_RESERVE`. The `PgListener` uses one of the `control` connections.

Each Compose service receives only the credentials it uses: only `bootstrap` sees the admin credentials, and `api`
only those of `app_runtime`. The migrator credentials are never present in the API container.

---

## Managed database (DB_BOOTSTRAP=false)

When a DBA creates the resources, `bootstrap` only validates them (using the migrator credentials). Required
privileges:

```sql
CREATE ROLE app_migrator LOGIN CREATEDB NOSUPERUSER NOCREATEROLE PASSWORD '...';
CREATE ROLE app_runtime  LOGIN NOCREATEDB NOSUPERUSER NOCREATEROLE PASSWORD '...';
CREATE DATABASE control OWNER app_migrator TEMPLATE template0 ENCODING 'UTF8';
REVOKE ALL ON DATABASE control FROM PUBLIC;
GRANT CONNECT ON DATABASE control TO app_migrator, app_runtime;
```

- `app_migrator` needs `CREATEDB` (it creates one database per tenant and owns the databases and objects). It needs
  neither superuser nor `CREATEROLE`.
- `app_runtime` only needs `LOGIN`. All other privileges (per table and per column) are granted by `migrate` and by
  provisioning.
- PostgreSQL 15 or later (the `public` schema belongs to the database owner).

Validation fails if `app_runtime` has `CREATEDB` or if either role is a superuser.

---

## Moving from the container database to an external one

1. Stop the application: `make down` (the PostgreSQL volume stays).
2. Start only the project database (`make db-up`) and dump **every application database** (`control` and each
   `tenant_*`), including roles:
   ```bash
   docker compose --env-file .env.docker exec -T postgres pg_dumpall -U "$DB_ADMIN_USER" --roles-only > roles.sql
   for db in control $(docker compose --env-file .env.docker exec -T postgres psql -U "$DB_ADMIN_USER" -Atc \
       "select datname from pg_database where datname like 'tenant_%'"); do
     docker compose --env-file .env.docker exec -T postgres pg_dump -U "$DB_ADMIN_USER" -Fc "$db" > "$db.dump"
   done
   ```
3. On the target: create the roles (from `roles.sql`, or by the DBA as in the previous section), create each database
   with `OWNER app_migrator TEMPLATE template0 ENCODING 'UTF8'` and restore with
   `pg_restore --no-owner --role=app_migrator -d <database>`.
4. Reapply the `CONNECT` restrictions and privileges: run `bootstrap` (or validate with `DB_BOOTSTRAP=false`) and then
   `migrate`, which reapplies the grants on `control` and on every tenant. For the tenant databases, also run
   `REVOKE ALL ON DATABASE tenant_x FROM PUBLIC; GRANT CONNECT ON DATABASE tenant_x TO app_runtime;`.
5. Copy `.env.docker` to `.env.external`, adjust `DB_HOST`, `DB_PORT`, `DB_SSLMODE`, the CA in `./certs` and
   `MAX_OPEN_TENANT_POOLS` according to the target's `max_connections`, and start with `make up-external`.

The `tenant_<uuid>` names are stored in `tenants.db_name` and must be kept.

---

## Architecture and technical decisions

```
src/main.rs              binary commands
src/app.rs               router, global middlewares, /metrics, serve
src/config.rs            environment-only configuration; Secret never shows in Debug
src/state.rs             AppState, cache invalidation and PgListener
src/error.rs             RFC 9457 (application/problem+json)
src/openapi.rs           OpenAPI specification (utoipa) and Swagger UI
src/password.rs          Argon2id in spawn_blocking + Semaphore
src/auth/                jwt, login/refresh/logout, access middleware, rate limit
src/tenancy/             tenant registry, per-tenant pools, tenant resolution, provisioning
src/access/              catalog, profiles, overrides, permission resolution and caches
src/users/               user CRUD
src/platform/            platform endpoints
src/cli/                 bootstrap, migrate, seed, worker, healthcheck
migrations/control/      control schema (+ NOTIFY triggers)
migrations/tenant/       schema of each tenant
```

### Multi-tenancy

- A `control` database with tenants, owner/superusers, sessions and the platform audit log; one
  `tenant_<uuid without hyphens>` database per tenant, with no `tenant_id` in the tables.
- **There is no default pool and no fallback.** All tenant data access goes through `TenantCtx`, created only by the
  middleware after resolving the tenant; repositories explicitly receive that pool's executor.
- Tenant token: the tenant comes only from the signed `tid` claim; a mismatching `X-Tenant-Slug` → 403. Platform
  token: requires `X-Tenant-Slug`. Only `active` tenants are served; suspended → 403 for everyone (including login).
- Tenant registry in `moka` (30 s TTL), by id and by slug, with negative caching. Invalidated by a trigger
  (`AFTER INSERT OR UPDATE ON tenants` → `pg_notify`), delivered only on `COMMIT`.
- Per-tenant pools: lazy `PgPool` in `moka` with `EvictionPolicy::lru()`, `try_get_with`, 10 min `time_to_idle`,
  `min_connections=0`, 60 s `idle_timeout`, 1 s `acquire_timeout` (→ 503) and a 2 s `statement_timeout` sent in the
  startup packet (no extra round trip).
- **Evicting a pool without breaking requests:** the pool lives in an `Arc<TenantPool>`; the `Drop` of the last `Arc`
  closes the pool in a separate task. Requests that already hold the handle finish normally, and the cache frees the
  slot immediately.

### PostgreSQL roles

- `app_migrator` (owns everything, `CREATEDB`) used only by `migrate`, `seed`, `worker` and bootstrap.
- `app_runtime` (API) on `control`: `tenants` SELECT/INSERT and UPDATE only on `name, status, last_error, updated_at`;
  `platform_users` SELECT/INSERT/DELETE and UPDATE only on `name, password_hash, is_active, updated_at`;
  `platform_refresh_tokens` DML; `platform_audit` SELECT/INSERT.
- On tenants: DML on the tables, `access_audit` SELECT/INSERT only, and `modules`/`features` SELECT only (the catalog
  is read-only for the API). `REVOKE ALL ON DATABASE ... FROM PUBLIC` (including `TEMP`) and `CONNECT` only for
  `app_runtime`.
- Bootstrap sends **SCRAM-SHA-256 verifiers** computed on the client (`PASSWORD 'SCRAM-SHA-256$...'`): the plaintext
  password never appears in SQL or in logs, not even on the server side.

### Authorization (deny by default)

Every route is declared as `RouteDef { method, path, access, handler }`, with `Access::{Public, Platform,
PlatformOwner, TenantToken, Feature(f), SelfOr(f)}`, and the router applies the `enforce` middleware to each one.
Handlers get the context through an extractor; without the middleware the extractor fails with 500 (it never grants
access). A test walks every route and checks 401 without a token, 403 without the feature, 403 for a superuser on an
owner route and 403 for a tenant token on a platform route.

- Effective permissions are resolved in a single database round trip (the user + the features query as an
  `ARRAY(...)` subquery).
- Immutable `Arc<Principal>` per `(tenant_id, user_id)` with `is_active`, `is_tenant_admin` and a **`u64` bitset** of
  features: O(1) checks with no hashing or allocation.
- Separate platform cache (`role`, `is_active`). **Owner routes read `role`/`is_active` straight from `control`, with
  no cache.**
- 30 s TTL, configurable capacity, coalescing with `try_get_with`. A user change invalidates its entry; a profile
  change invalidates the whole tenant (`invalidate_entries_if`).
- After commit: invalidates locally and publishes `pg_notify('cache_invalidation', ...)` on `control` (NOTIFY is
  per database). A publish failure → log + `cache_invalidation_publish_failures_total` metric; the TTL bounds the
  staleness.
- A single `PgListener` per instance, with `eager_reconnect`; on reconnect, it clears every cache.

### Privilege escalation protections

- A single owner (unique partial index), created only by the seed; no route has a `role` field, and every superuser
  query filters on `role = 'superuser'`.
- `is_tenant_admin` only changes through owner endpoints; unknown fields are rejected (422).
- Users without full access can only grant features they have — through a profile (only what is **added** is
  checked), an `allow` override, and also removing a `deny` (which is equivalent to granting).
- A regular user can't change their own profiles/overrides; a tenant admin can only be changed/deleted by a platform
  user (the admin can change their own name and password).
- Last active admin: the `UPDATE`/`DELETE` runs first and, if the target was an active admin, the transaction takes
  `pg_advisory_xact_lock` and checks whether any remains. Under READ COMMITTED this serializes the races correctly
  (tested with concurrent removals).

### Authentication

- 15 min HS256 JWT, fixed algorithm, required `exp`/`iss`/`aud`/`sub`, distinct secrets and audiences for the platform
  (`simple-core:platform`) and tenants (`simple-core:tenant`). No role or permissions in the token.
- Opaque 256-bit refresh tokens, stored as SHA-256, 30 days, rotated on every use; reusing a revoked token revokes
  all of the user's sessions. Changing the password or deactivating the user also revokes the sessions.
- Browser clients send `X-Auth-Mode: cookie`: login and refresh put the refresh token in an `HttpOnly; Secure;
  SameSite=Strict` cookie (`sc_platform_rt` on `/v1/platform/auth`, `sc_tenant_rt` on `/v1/auth`) instead of the body,
  and refresh and logout read it from there. The required header also blocks CSRF (no CORS is enabled). One tenant
  session per browser: logging into another tenant replaces the cookie.
- Logins: the same 401 for a nonexistent tenant, nonexistent e-mail, wrong password and inactive account, always
  running Argon2id (with a dummy hash when there is no user).
- Login rate limit per scope (platform or tenant), IP and e-mail, in a fixed window (`moka` with TTL) → 429.

### Performance and resilience

- Argon2id `m=19456, t=2, p=1` in `spawn_blocking`, a `Semaphore` sized to the number of cores, max wait 1 s → 503.
- No global lock on the request path; with warm caches, tenant resolution and authorization don't hit the database.
- JSON logs via non-blocking `tracing-appender`, no per-request log at `info`.
- Metrics: latency per route (`route` = the route pattern, never the tenant), open pools, open/busy connections, hash
  queue, provisioning queue, hits/misses per cache, invalidations, timeouts and panics.
- 3 s global timeout (503), 16 KB max body, `CatchPanicLayer` (with `panic="unwind"` the process doesn't crash),
  graceful shutdown on SIGTERM.
- Release with `opt-level=3`, `lto="fat"`, `codegen-units=1`, `strip=true`, `panic="unwind"`.
- Keyset pagination (`WHERE id > $1 ORDER BY id LIMIT $2`), explicit columns, direct `INSERT` with 23505 → 409,
  `UPDATE ... RETURNING` in one query, `rows_affected` on `DELETE`, UUIDv7 generated by the application.

### Provisioning

`seed` and `worker` share the same resumable routine: per-tenant advisory lock on a dedicated connection → revalidate
`db_name` → `CREATE DATABASE ... TEMPLATE template0 ENCODING 'UTF8'` if missing (escaped identifier) → `CONNECT` and
privileges → tenant migrations → catalog (upsert by `code`, in one transaction) → `active`. On error: `failed` with a
short `last_error` containing no secrets; `POST /v1/platform/tenants/{id}/retry` moves it back to `provisioning`. The
worker claims tenants with `FOR UPDATE SKIP LOCKED` and is woken up by the trigger that publishes on
`tenant_provisioning`.

Migrations are embedded in the binary (`sqlx::migrate!`) and must follow expand/contract; the catalog never removes
features.

### SQLx with two schemas

`make sqlx-prepare` creates the temporary `sqlx_check` database, applies `migrations/control` and `migrations/tenant`
(they share no tables), runs `cargo sqlx prepare` and drops the database. `make sqlx-check` fails if `.sqlx` is out of
date. The image builds with `SQLX_OFFLINE=true`, without a database.

---

## Tests

```bash
make db-up
make test          # or: cargo test (reads .env.local if DB_HOST is not set)
make fmt lint sqlx-check
```

Each integration test creates its own `control` (`test_<uuid>`) and tenants on a real PostgreSQL, so tests run in
parallel; the databases are dropped at the end. Coverage (43 tests: 13 unit and 30 integration):

- **Platform** (`tests/platform.rs`): only the owner creates tenants/superusers/admins (superuser, admin and regular
  user → 403); a superuser can enter every tenant but can't change `is_tenant_admin`; nobody creates, changes or
  deletes the owner, including concurrent requests and SQL with `app_runtime`; a deactivated superuser loses access on
  both instances; token scopes; a sweep over every route; rate limit; `me` and superusers.
- **Isolation** (`tests/isolation.rs`): the same e-mail in two tenants and on the platform; a tenant A token has no
  access to B (including with another `X-Tenant-Slug` and refresh); an admin is restricted to their own tenant; fast
  suspension and reactivation on both instances; `app_runtime` has no DDL, no `CREATE DATABASE`, can't change
  `db_name`/`role`/audit logs and has no `TEMP`; database ACL without `PUBLIC`.
- **Error codes** (`tests/errors.rs`): every Problem has a `code`; specific codes for 403/409 and per-field codes in 422,
  documented in the specification.
- **Refresh cookie** (`tests/auth_cookie.rs`): `X-Auth-Mode: cookie` sets `HttpOnly`/`SameSite=Strict` cookies and hides
  the token from the body; rotation, reuse, logout and clearing the cookie; the cookie is ignored without the header.
- **Access** (`tests/access.rs`): CRUD with 404/409/422/413 and concurrent duplicate e-mails; keyset pagination;
  permissions from a profile, union of profiles and `allow`; `deny` wins and expired overrides are ignored;
  inactive/401/403; self-service name and password; invalidation across two instances affecting only the right
  tenant; escalation protections; last-admin race; refresh token rotation and reuse; catalog, profiles and audit.
- **Operations** (`tests/operation.rs`): seed run twice and four times in parallel (one owner, one tenant, password
  and database OID unchanged; a superuser e-mail is not promoted); two workers competing for the same tenant;
  provisioning interrupted at each of the 5 steps and resumed; `migrate` with one failing tenant not affecting the
  others and returning an error; evicting a pool from the cache with a query in flight and LRU with capacity 2;
  idempotent bootstrap and validation mode.
- **Documentation** (`tests/docs.rs` and `src/openapi.rs`): `/docs` and `/openapi.json` only exist with
  `API_DOCS=true` and don't open up API routes; `api openapi` prints the same specification that is served; the
  specification covers exactly the router's routes, with security
  and parameters consistent with each route's `Access`.

---

## Load tests

```bash
make up                                                     # stack with the release image
docker compose --env-file .env.docker -f compose.yaml -f load/compose.load.yaml \
  --profile db --profile app up -d --wait                   # applies the load CPU pinning
make load-populate                                          # 100 tenants, 1M+99k users, profiles, overrides
make load                                                   # every scenario (≈ 40 min)
make load-scenario S=get_by_id RATE=15000                   # a single scenario
```

- **Data** (`load/populate.sh`): 100 tenants created through the API and provisioned by the worker; `load-000` with
  1,000,000 users and the other 99 with 1,000 each; 20 profiles per tenant; 1 or 2 profiles per user; overrides on 5%
  of the users; real Argon2id passwords. Tokens for 5,950 distinct users (1,000 in the large tenant + 50 in each small
  one), signed by k6 with the tenant secret.
- **CPU isolation** (`load/compose.load.yaml`): API on `cpuset 0-3`, PostgreSQL on `4-7`, k6 on `8-15` — separate
  physical cores (SMT: 0-1, 2-3, ...). `fsync` and `synchronous_commit` on.
- **Method:** constant arrival rate (k6 `constant-arrival-rate`), 30 s warm-up and 3 min of measurement; the summary
  covers only the measurement. If the API can't keep up, the measured RPS falls below the requested rate. CPU/memory
  from `docker stats`, connections from `pg_stat_activity`, cache hit ratios from the difference of the `/metrics`
  counters.
- **Mix**: 80% `GET` by id, 15% list, 4% name `PATCH` and 1% `DELETE` (victims created before each run, deleted only
  once). In "all tenants", Zipf traffic (tenant *i* gets weight 1/(i+1)).
- Only in the load override are login limits high: the login scenario measures Argon2id, not the rate limit.

### Hardware

| Item | Value |
|---|---|
| CPU | AMD Ryzen 7 9800X3D (8 cores / 16 threads) |
| RAM | 32 GB (Docker VM: 16 GB, 16 vCPUs) |
| OS | Windows 11 Pro + Docker Desktop 29.5 (WSL2) |
| API | 4 vCPUs = 2 physical cores with SMT (`cpuset 0-3`), 2 GB limit |
| PostgreSQL 18 | 4 vCPUs = 2 physical cores (`cpuset 4-7`), `shared_buffers=1GB`, `max_connections=1150` |
| k6 | 4 physical cores (`cpuset 8-15`), on the same bridge network |

Note: "4 vCPUs" here means 2 physical cores with SMT, and Windows keeps scheduling host work on the same cores as the
VM. Repeated runs of the same scenario varied considerably (see below), especially at p99.

## Results

Final configuration: release image (`lto="fat"`, mimalloc), `TENANT_POOL_MAX_CONNECTIONS=10`,
`MAX_OPEN_TENANT_POOLS=110`, `CONTROL_POOL_MAX_CONNECTIONS=10`, PostgreSQL with `max_connections=1150`,
`fsync`/`synchronous_commit` on. Each row: 30 s warm-up + 3 min of measurement, constant arrival rate.
Raw data in `load/results/30-final/` (k6 JSON, CPU/memory/connection samples and cache counters).

| Scenario | Requested rate | RPS | p50 ms | p95 ms | p99 ms | Errors | API CPU avg/max | API mem max | PG CPU avg/max | PG mem max | Runtime connections avg/max | Cache hit ratio |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| get_by_id | 15000 | 14994 | 0.40 | 0.97 | 7.25 | 0.000% | 282% / 299% | 69 MiB | 178% / 191% | 482 MiB | 12 / 12 (max 3 active) | tenant_pool 100.00% tenant_registry 100.00% access 99.78% |
| list | 4000 | 4000 | 0.49 | 0.64 | 0.75 | 0.000% | 98% / 112% | 72 MiB | 71% / 81% | 605 MiB | 12 / 12 (max 2 active) | tenant_pool 100.00% tenant_registry 100.00% access 99.17% |
| mix_single | 9000 | 8989 | 0.44 | 2.43 | 14.01 | 0.000% | 211% / 235% | 72 MiB | 142% / 161% | 710 MiB | 20 / 20 (max 5 active) | tenant_pool 100.00% tenant_registry 100.00% access 99.63% |
| mix_all | 9000 | 9000 | 0.46 | 2.08 | 3.07 | 0.000% | 213% / 225% | 103 MiB | 161% / 189% | 2862 MiB | 688 / 691 (max 6 active) | tenant_pool 100.00% tenant_registry 99.96% access 97.72% |
| pool_churn | 8000 | 947 | 765.89 | 1532.24 | 1592.33 | 0.060% | 191% / 201% | 157 MiB | 382% / 389% | 1408 MiB | 137 / 237 (max 3 active) | tenant_pool 41.13% tenant_registry 99.65% access 84.39% |
| noisy_neighbor (normal tenants) | 1500 | 1500 | 0.44 | 0.76 | 1.18 | 0.000% | 163% / 196% | 115 MiB | 98% / 120% | 2715 MiB | 724 / 724 (max 1 active) | tenant_pool 100.00% tenant_registry 99.96% access 97.88% |
| noisy_neighbor (noisy tenant, 6000/s) | 1500 | 6000 | 0.40 | 0.57 | 0.95 | 0.000% | 163% / 196% | 115 MiB | 98% / 120% | 2715 MiB | 724 / 724 (max 1 active) | tenant_pool 100.00% tenant_registry 99.96% access 97.88% |
| login | 150 | 150 | 11.15 | 13.73 | 16.02 | 0.000% | 160% / 166% | 186 MiB | 10% / 21% | 2740 MiB | 238 / 729 (max 1 active) | tenant_pool 100.00% tenant_registry 99.98% |
| create | 150 | 150 | 12.28 | 34.99 | 389.73 | 0.326% | 164% / 182% | 190 MiB | 10% / 13% | 950 MiB | 20 / 20 (max 10 active) | tenant_pool 100.00% tenant_registry 99.98% access 81.22% |
| read_with_invalidations | 10000 | 9999 | 0.43 | 0.80 | 2.47 | 0.000% | 224% / 241% | 111 MiB | 149% / 157% | 954 MiB | 20 / 20 (max 3 active) | tenant_pool 100.00% tenant_registry 100.00% platform 98.34% access 80.17% |
| read_with_provisioning | 10000 | 10000 | 0.39 | 0.58 | 1.48 | 0.000% | 214% / 228% | 115 MiB | 129% / 144% | 1154 MiB | 20 / 20 (max 2 active) | tenant_pool 100.00% tenant_registry 100.00% access 99.67% |

CPU as % of one vCPU (400% = all 4 vCPUs). "Connections" = `app_runtime` connections in `pg_stat_activity`.
"Cache hit ratio" = hits/(hits+misses) during the measurement, per cache.

### Targets

| Target | Result | |
|---|---|---|
| `GET` by id, one tenant, warm caches: ≥ 15,000 RPS, p99 < 10 ms, no errors | 14,994 RPS (15,000 requested, no meaningful drops), p99 7.25 ms, 0 errors | met |
| Mix on one tenant: ≥ 8,000 RPS, p99 < 25 ms, errors < 0.1% | 8,989 RPS (9,000 requested), p99 14.0 ms, 0 errors | met |

The test uses a constant rate: RPS follows the requested rate and does not measure the maximum capacity above it. In
`get_by_id` the API sat at ~280% CPU (out of 400%), so there is some headroom, but it wasn't measured.

### Optimization history (one change at a time)

| # | Change | Scenario | Before | After | Decision |
|---|---|---|---|---|---|
| 01 | Baseline (pool of 10, default `test_before_acquire`) | `get_by_id` 15k/s | — | 12,926 RPS, p99 115 ms; repeat: 14,735 RPS, p99 68 ms | — |
| 02 | `test_before_acquire(false)`: sqlx was pinging PostgreSQL on every `acquire` | `get_by_id` 15k/s | p95 50.7 ms, p99 68.1 ms, API CPU 329%, PG 220% | 14,854 RPS, p95 3.0 ms, p99 29.6 ms, API CPU 290%, PG 181% | kept |
| 03 | mimalloc vs. default allocator (latency) | `get_by_id` ×2, `mix_single` | p99 29.6 / 14.3 ms; mix p99 3.18 ms | p99 19.8 / 27.8 ms; mix p99 3.14 ms | no difference beyond noise |
| 04 | Pools for every active tenant: 3 connections × 120 pools (was 10 × 25) | `mix_all` 9k/s | 1,873 RPS, p99 1,520 ms, **78% errors** (ephemeral ports exhausted by pool churn: `Cannot assign requested address`) | 8,985 RPS, p99 6.5 ms, 0% | root cause confirmed, see #07 |
| 05 | `net.ipv4.tcp_tw_reuse=1` in the API container | `pool_churn` | 1,832 RPS, **86.7% errors** | 866–947 RPS, 0.06–0.4% errors | kept |
| 06 | mimalloc (memory) | `login` 150/s | API RSS 1,493 MiB (glibc arena fragmentation from Argon2id's 19 MiB buffers across many `spawn_blocking` threads) | 133 MiB, same latency | **mimalloc kept** (default feature) |
| 07 | 3 connections per tenant were too few for the hot tenant | `get_by_id` | 3 connections: 8,568 RPS, p50 92 ms (pool queueing, API CPU 200%) | 6 connections: p99 63 ms; 10 connections: p99 10.7 ms | back to 10 × 110 pools, `max_connections=1150` |

The load generator was fixed too: signing JWTs per VU and copying the tenants into every VU kept k6 at 388% CPU and
4 GB of memory, capping `mix_all` at 4,322 RPS. Tokens are now signed once, in a `SharedArray`.

Lessons: sampling `pg_stat_activity` every 5 s (max 5 active connections) underestimated the real concurrency in the
pool; the pool size decision was only right after measuring latency with 3, 6 and 10 connections. There is also
considerable variance between identical runs on this machine (01 vs. its repeat), especially at p99.

### Bottlenecks and observations

- **Reads on one tenant (`get_by_id`)**: API CPU (~280–300% of 400%) and PostgreSQL CPU (~180%). With warm caches,
  tenant resolution and authorization don't hit the database (99.8% hit ratio on `access`, 100% on registry and
  pools); what remains is one primary-key query per request.
- **Pool churn (`pool_churn`, 20 pools for 100 tenants with uniform traffic)**: ~80% of requests open a new
  connection. The limit is **PostgreSQL saturated (≈ 385% CPU) opening connections** (backend fork + SCRAM), at
  ~950 RPS and p99 ~1.6 s. `tcp_tw_reuse` removed the port-exhaustion errors, but throughput stays limited.
  **No target applies here, but the result is inherently poor**: in practice, `MAX_OPEN_TENANT_POOLS` must cover the
  active tenants (as in `mix_all`, which runs clean); reducing this cost would need an external pooler (PgBouncer,
  out of scope).
- **Mix across all tenants (Zipf)**: 9,000 RPS, p99 3.1 ms, no errors. The cost is the number of open connections
  (~690 with 100 active tenants): pools are lazy, but connections of tenants with traffic stay open for up to 60 s
  idle.
- **Noisy neighbor**: with one tenant at 6,000 req/s, the other 98 tenants (1,500 req/s combined) stayed at a p99 of
  1.2 ms. Each tenant has its own pool, so a hot tenant doesn't exhaust the others' connections.
- **Invalidations during reads** (the whole tenant invalidated 2×/s): 9,999 RPS, p99 2.5 ms. The `access` cache hit
  ratio drops to 80%, and `try_get_with` coalescing prevents a reload stampede.
- **Provisioning during reads** (a new tenant every 10 s): 10,000 RPS, p99 1.5 ms, no visible impact.
- **Login and POST**: intentionally bound by Argon2id (`m=19456,t=2,p=1`, ~11 ms per hash). At 150 req/s, login had a
  p99 of 16 ms and 0 errors. `POST /v1/users` had a p99 of 15 ms and 0 errors in the previous run (`20-final`), but in
  this one a p99 of 390 ms and 0.33% errors (503 from waiting > 1 s on the hash semaphore) with the same hash
  configuration: a sign of transient CPU interference (the Windows host shares cores with the VM). The Argon2id
  parameters were never lowered to improve the benchmark.
- **Memory**: API between 70 and 190 MiB across all scenarios (with mimalloc).

### Environment limitations

- Docker Desktop on Windows (WSL2 VM): the host keeps using the same cores, which increases variance.
- "4 vCPUs" = 2 physical cores with SMT, for both the API and PostgreSQL.
- k6 on the same machine, on separate cores, talking to the API over Docker's bridge network.

---

## Releases and versioning

Versioning is automatic, with [release-please](https://github.com/googleapis/release-please)
(`.github/workflows/release.yml`), based on the commit messages
([Conventional Commits](https://www.conventionalcommits.org/)):

| Commit | Next version (while < 1.0.0) |
|---|---|
| `feat: ...` | minor: 0.1.0 → 0.2.0 |
| `fix: ...`, `perf: ...` | patch: 0.2.0 → 0.2.1 |
| `feat!: ...` or a `BREAKING CHANGE:` footer | minor (major after 1.0.0) |
| `docs:`, `chore:`, `ci:`, `test:`, `refactor:` | no release on their own (`docs` shows in the changelog) |

1. Every push to `main` opens or updates a pull request named `chore(main): release X.Y.Z`. It bumps the version in
   `Cargo.toml`/`Cargo.lock` and updates `CHANGELOG.md`.
2. Merging that pull request creates the `vX.Y.Z` tag and the GitHub release with the release notes.
3. The `binary` job then builds the release binary and attaches
   `<repo>-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` (plus its `.sha256`) to the release.

The repository needs *Settings → Actions → General → Workflow permissions → Allow GitHub Actions to create and
approve pull requests* enabled.

---

## Starting a new project from this base

`scripts/new-project.sh` creates a clean, renamed copy of this API to start a new system:

```bash
scripts/new-project.sh billing-api                 # creates ../billing-api
scripts/new-project.sh billing-api ~/code/billing  # custom destination
scripts/new-project.sh billing-api --check         # also builds and runs the unit tests
```

- **Kept:** everything that works — authentication, tenants, users, profiles and permissions, platform endpoints,
  CLI commands, Swagger, migrations, Docker Compose, Makefile and all the tests.
- **Versioning:** the release workflow is kept; the new project starts from scratch (no `CHANGELOG.md`, first
  release 0.1.0).
- **Removed:** the load tests (`load/`), the performance results and history, the Portuguese README and the license
  files (the new project starts without a license).
- **Renamed:** `simple-core`/`simple_core` become the new name everywhere — package and crate, Compose project and
  volume, Docker image, JWT issuer and audiences, and the Swagger title.
- A short README is generated and a Git repository is created on `main` with one initial commit, using this
  repository's `user.name`/`user.email`.

The copy starts from the last commit (`HEAD`); uncommitted changes are not included. To run the new project next to
this one, change `API_PORT` and `DB_PUBLISH_PORT` in its `.env` file.

---

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT License ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this project by you,
as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
