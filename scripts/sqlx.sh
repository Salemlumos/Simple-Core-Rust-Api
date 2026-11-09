#!/usr/bin/env bash
# Gera (prepare) ou verifica (check) o diretório .sqlx.
#
# As queries verificadas em compilação usam um único banco de referência, mas
# o projeto tem dois schemas (control e tenant), sem tabelas em comum. Este
# script cria o banco temporário `sqlx_check`, aplica migrations/control e
# migrations/tenant nele, roda o `cargo sqlx prepare` e remove o banco.
#
# Requer o PostgreSQL do projeto (make db-up) e o sqlx-cli 0.9.
set -euo pipefail

MODE="${1:-prepare}"
ENV_FILE="${ENV_FILE:-.env.local}"
if [ -z "${__ENV_LOADED:-}" ]; then
  exec env __ENV_LOADED=1 bash scripts/with-env.sh "$ENV_FILE" bash "$0" "$@"
fi

CHECK_DB=sqlx_check
psql_admin() {
  docker compose --env-file "$ENV_FILE" exec -T postgres \
    psql -X -q -v ON_ERROR_STOP=1 -U "$DB_ADMIN_USER" "$@"
}
cleanup() { psql_admin -d postgres -c "DROP DATABASE IF EXISTS $CHECK_DB WITH (FORCE)" >/dev/null || true; }
trap cleanup EXIT

cleanup
psql_admin -d postgres -c "CREATE DATABASE $CHECK_DB TEMPLATE template0 ENCODING 'UTF8'"
for f in migrations/control/*.sql migrations/tenant/*.sql; do
  psql_admin -d "$CHECK_DB" < "$f"
done

export DATABASE_URL="postgres://${DB_ADMIN_USER}:${DB_ADMIN_PASSWORD}@127.0.0.1:${DB_PUBLISH_PORT:-5433}/${CHECK_DB}"
case "$MODE" in
  prepare) cargo sqlx prepare -- --all-targets ;;
  check)   cargo sqlx prepare --check -- --all-targets ;;
  *) echo "uso: $0 prepare|check" >&2; exit 2 ;;
esac
