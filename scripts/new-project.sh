#!/usr/bin/env bash
# Cria um projeto novo a partir deste, já renomeado e sem o que é específico do
# simple-core (testes de carga, resultados, histórico de desempenho, licenças).
#
# Mantém toda a base funcional: autenticação, tenants, usuários, perfis e
# permissões, plataforma, CLI (bootstrap/migrate/seed/serve/worker), Swagger,
# migrations, Docker Compose, Makefile e todos os testes.
#
# Uso:
#   scripts/new-project.sh <nome> [destino] [--check]
#
#   <nome>     nome do projeto em kebab-case, ex.: billing-api
#              (vira o pacote billing-api, a crate billing_api, o projeto do
#              Compose, a imagem Docker, o issuer do JWT e o título do Swagger)
#   [destino]  diretório a criar (padrão: ../<nome>, ao lado deste repositório)
#   --check    compila o projeto novo e roda os testes unitários no final
#
# A cópia parte do último commit (HEAD): alterações não commitadas não entram.
set -euo pipefail

usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit "${1:-0}"; }

NAME="" DEST="" CHECK=0
for arg in "$@"; do
  case "$arg" in
    -h|--help) usage 0 ;;
    --check) CHECK=1 ;;
    -*) echo "Opção desconhecida: $arg" >&2; usage 1 ;;
    *) if [ -z "$NAME" ]; then NAME="$arg"; elif [ -z "$DEST" ]; then DEST="$arg"; else usage 1; fi ;;
  esac
done
[ -n "$NAME" ] || usage 1
if ! [[ "$NAME" =~ ^[a-z][a-z0-9]*(-[a-z0-9]+)*$ ]]; then
  echo "Nome inválido: '$NAME'. Use kebab-case com letras minúsculas e números (ex.: billing-api)." >&2
  exit 1
fi
SNAKE="${NAME//-/_}"

SRC="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
DEST="${DEST:-$SRC/../$NAME}"
if [ -e "$DEST" ] && [ -n "$(ls -A "$DEST" 2>/dev/null)" ]; then
  echo "O destino '$DEST' já existe e não está vazio." >&2
  exit 1
fi
if [ -n "$(git -C "$SRC" status --porcelain)" ]; then
  echo "Aviso: há alterações não commitadas em $SRC; elas não serão copiadas." >&2
fi

mkdir -p "$DEST"
DEST="$(cd "$DEST" && pwd)"
echo "==> Copiando $SRC (HEAD) para $DEST"
git -C "$SRC" archive --format=tar HEAD | tar -x -C "$DEST"
cd "$DEST"

echo "==> Removendo o que é específico do simple-core"
rm -rf load LICENSE-APACHE LICENSE-MIT README.pt-BR.md scripts/new-project.sh
# Versionamento do zero: sem CHANGELOG e a primeira release do projeto novo será a 0.1.0.
rm -f CHANGELOG.md
printf '{\n  ".": "0.0.0"\n}\n' > .release-please-manifest.json
perl -0pi -e 's/^version = "[^"]*"/version = "0.1.0"/m' Cargo.toml
perl -0pi -e 's/(\nname = "simple-core"\nversion = )"[^"]*"/$1"0.1.0"/' Cargo.lock

# Makefile: sem os alvos de carga.
perl -0pi -e '
  s/\n# -+ carga\n.*\z/\n/s;
  s/^(up down logs reset) load-populate load load-scenario:/$1:/m;
  s/comandos de desenvolvimento, execução e carga\./comandos de desenvolvimento e execução./;
' Makefile
perl -ni -e 'print unless m{^/load/}' .gitignore
perl -ni -e 'print unless /^load$/' .dockerignore
# Sem licença: o dono do projeto novo decide.
perl -ni -e 'print unless /^license = /' Cargo.toml

echo "==> Renomeando simple-core -> $NAME (crate $SNAKE)"
find . -type f ! -path './.git/*' ! -path './target/*' -print0 |
  while IFS= read -r -d '' f; do
    if grep -Iq . "$f" 2>/dev/null && grep -q -e 'simple-core' -e 'simple_core' "$f"; then
      perl -pi -e "s/simple_core/$SNAKE/g; s/simple-core/$NAME/g" "$f"
    fi
  done

# O nome novo muda a ordem dos imports e o comprimento das linhas.
if command -v cargo >/dev/null; then cargo fmt; else echo "Aviso: cargo não encontrado; rode cargo fmt depois." >&2; fi

echo "==> Gerando README.md"
cat > README.md <<EOF
# $NAME

Multi-tenant REST API with access control, built in Rust (Axum + PostgreSQL).

- Database per tenant, a \`control\` database for the platform
- Four user levels: owner, superuser, tenant admin and regular user
- Access by modules, features and profiles, with per-user allow/deny overrides (deny always wins)
- Argon2id passwords, short-lived JWTs and rotating refresh tokens
- OpenAPI 3.1 with Swagger UI at \`/docs\` (\`API_DOCS=true\`)
- Docker Compose (\`db\` and \`app\` profiles), integration tests against a real PostgreSQL

A single binary, \`api\`, with the commands \`bootstrap\`, \`migrate\`, \`seed\`, \`serve\`, \`worker\` and \`healthcheck\`.

## Quick start

Requirements: Docker with Compose 2.20+ and GNU make (Rust only for the local modes).

\`\`\`bash
cp .env.docker.example .env.docker   # fill in every password and secret (openssl rand -base64 48)
make up                              # PostgreSQL, bootstrap, migrate, seed, API and worker
curl localhost:8080/health/ready
open http://localhost:8080/docs      # Swagger UI
make down                            # stop (keeps the data); make reset deletes the database volume
\`\`\`

| API and worker | PostgreSQL | Command |
|---|---|---|
| Container | Project container | \`make up\` |
| Local machine | Project container | \`make db-up\`, \`make dev-init\`, \`make dev-api\` and \`make dev-worker\` (\`.env.local\`) |
| Container | External | \`make up-external\` (\`.env.external\`) |

Log in with the owner from \`SEED_OWNER_*\` at \`POST /v1/platform/auth/login\`, then create the first tenant admin at
\`POST /v1/platform/tenants/{id}/admins\`. Tenant users log in at \`POST /v1/auth/login\` with the \`X-Tenant-Slug\`
header.

## Development

\`\`\`bash
make db-up
make test                 # unit + integration tests (real PostgreSQL)
make fmt lint sqlx-check
make sqlx-prepare         # after changing any sqlx::query! macro
make help                 # every target
\`\`\`

GitHub Actions (`.github/workflows/ci.yml`) runs `fmt`, `lint`, `sqlx-check` and `test` against a real
PostgreSQL on every pull request and push to `main`.

## Releases

Versioning is automatic ([release-please](https://github.com/googleapis/release-please)): write commits as
Conventional Commits (\`feat:\`, \`fix:\`, \`perf:\`, \`docs:\`...). Every push to \`main\` opens or updates a release PR with
the next version and the CHANGELOG; merging it creates the \`vX.Y.Z\` tag, the GitHub release and attaches the Linux
binary. Enable *Settings → Actions → General → Allow GitHub Actions to create and approve pull requests*.

## Structure

\`\`\`
src/main.rs              binary commands
src/app.rs               router, global middlewares, /metrics, serve
src/config.rs            environment-only configuration
src/openapi.rs           OpenAPI specification and Swagger UI
src/auth/                jwt, login/refresh/logout, access middleware, rate limit
src/tenancy/             tenant registry, per-tenant pools, provisioning
src/access/              catalog, profiles, overrides, permission resolution and caches
src/users/               user CRUD
src/platform/            platform endpoints
src/cli/                 bootstrap, migrate, seed, worker, healthcheck
migrations/control/      control schema
migrations/tenant/       schema of each tenant
\`\`\`

New endpoints are declared as a \`RouteDef\` with their \`Access\` rule in the module's \`routes.rs\`, annotated with
\`#[utoipa::path]\` and listed in \`src/openapi.rs\`; a unit test fails if the router and the documentation diverge.
New features go into the catalog in \`src/access/catalog.rs\`.
EOF

echo "==> Criando o repositório Git"
git init -q -b main
NAME_CFG="$(git -C "$SRC" config user.name || true)"
EMAIL_CFG="$(git -C "$SRC" config user.email || true)"
[ -n "$NAME_CFG" ] && git config user.name "$NAME_CFG"
[ -n "$EMAIL_CFG" ] && git config user.email "$EMAIL_CFG"
git add -A
git commit -q -m "chore: projeto inicial $NAME"

if [ "$CHECK" = 1 ]; then
  echo "==> Verificando (build + testes unitários)"
  SQLX_OFFLINE=true cargo test --locked --lib
fi

LEFT="$(grep -rIl -e 'simple-core' -e 'simple_core' . --exclude-dir=.git --exclude-dir=target || true)"
[ -z "$LEFT" ] || { echo "Aviso: ainda há referências ao nome antigo em:" >&2; echo "$LEFT" >&2; }

cat <<EOF

Projeto criado em $DEST

Próximos passos:
  cd "$DEST"
  cp .env.docker.example .env.docker   # preencha as senhas e segredos
  make up                              # e abra http://localhost:8080/docs

Para publicar (crie antes o repositório vazio no GitHub):
  git remote add origin https://github.com/<usuario>/<repositorio>.git
  git push -u origin main
EOF
