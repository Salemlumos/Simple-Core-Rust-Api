#!/usr/bin/env bash
# Popula a carga: 100 tenants (1 com 1.000.000 de usuários, 99 com 1.000),
# 20 perfis por tenant, exceções em 5% dos usuários e ids para os tokens
# (>= 1.000 usuários distintos). Idempotente.
#
# Requer a stack no ar (make up) com a API publicada em 127.0.0.1:${API_PORT}.
set -euo pipefail
cd "$(dirname "$0")/.."

ENV_FILE="${ENV_FILE:-.env.docker}"
if [ -z "${__ENV_LOADED:-}" ]; then
  exec env __ENV_LOADED=1 bash scripts/with-env.sh "$ENV_FILE" bash "$0" "$@"
fi

API="http://127.0.0.1:${API_PORT:-8080}"
BIG_USERS="${BIG_USERS:-1000000}"
SMALL_USERS="${SMALL_USERS:-1000}"
TENANTS="${TENANTS:-100}"
BIG_TOKENS=1000
SMALL_TOKENS=50
BIG_TARGETS=20000
SMALL_TARGETS=200
LOAD_PASSWORD="load-password-123"
DATA=load/data
mkdir -p "$DATA"

psql_db() { docker compose --env-file "$ENV_FILE" exec -T postgres psql -X -q -At -v ON_ERROR_STOP=1 -U "$DB_ADMIN_USER" -d "$@"; }
json_field() { sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }

echo "== login do owner"
OWNER=$(curl -sf -X POST "$API/v1/platform/auth/login" -H 'content-type: application/json' \
  -d "{\"email\":\"$SEED_OWNER_EMAIL\",\"password\":\"$SEED_OWNER_PASSWORD\"}" | json_field access_token)
[ -n "$OWNER" ] || { echo "falha no login do owner"; exit 1; }

echo "== criando $TENANTS tenants pela API (o worker provisiona)"
for i in $(seq -f "%03g" 0 $((TENANTS - 1))); do
  code=$(curl -s -o /dev/null -w '%{http_code}' -X POST "$API/v1/platform/tenants" \
    -H "authorization: Bearer $OWNER" -H 'content-type: application/json' \
    -d "{\"slug\":\"load-$i\",\"name\":\"Load $i\"}")
  case "$code" in 202|409) ;; *) echo "tenant load-$i: HTTP $code"; exit 1;; esac
done
echo -n "aguardando provisionamento"
for _ in $(seq 1 600); do
  pending=$(psql_db "$DB_CONTROL_NAME" -c "SELECT count(*) FROM tenants WHERE slug LIKE 'load-%' AND status <> 'active'")
  [ "$pending" = "0" ] && break
  echo -n "."; sleep 1
done
echo
[ "$pending" = "0" ] || { echo "tenants não ficaram ativos"; exit 1; }

# Hash Argon2id real da senha de carga (gerado pela própria API).
BIG_ID=$(psql_db "$DB_CONTROL_NAME" -c "SELECT id FROM tenants WHERE slug = 'load-000'")
BIG_DB=$(psql_db "$DB_CONTROL_NAME" -c "SELECT db_name FROM tenants WHERE slug = 'load-000'")
HASH=$(psql_db "$BIG_DB" -c "SELECT password_hash FROM users WHERE email = 'hash-source@load.test'")
if [ -z "$HASH" ]; then
  curl -sf -o /dev/null -X POST "$API/v1/platform/tenants/$BIG_ID/admins" \
    -H "authorization: Bearer $OWNER" -H 'content-type: application/json' \
    -d "{\"email\":\"hash-source@load.test\",\"name\":\"Hash\",\"password\":\"$LOAD_PASSWORD\"}"
  HASH=$(psql_db "$BIG_DB" -c "SELECT password_hash FROM users WHERE email = 'hash-source@load.test'")
fi

populate_tenant() { # db users tokens
  psql_db "$1" -v hash="$HASH" -v n="$2" -v k="$3" <<'SQL'
BEGIN;
INSERT INTO profiles (id, name, description)
SELECT uuidv7(), 'profile-' || lpad(g::text, 2, '0'), 'carga' FROM generate_series(1, 19) g
ON CONFLICT (name) DO NOTHING;
INSERT INTO profiles (id, name, description) VALUES (uuidv7(), 'load-operator', 'carga: operador')
ON CONFLICT (name) DO NOTHING;
INSERT INTO profile_features (profile_id, feature_id)
SELECT p.id, f.id FROM profiles p JOIN features f
  ON f.code IN ('users.read', 'users.create', 'users.update', 'users.delete')
WHERE p.name = 'load-operator'
ON CONFLICT DO NOTHING;
INSERT INTO profile_features (profile_id, feature_id)
SELECT p.id, f.id FROM profiles p CROSS JOIN features f
WHERE p.name LIKE 'profile-%' AND (f.code = 'users.read' OR abs(hashtext(p.name || f.code)) % 3 = 0)
ON CONFLICT DO NOTHING;
INSERT INTO users (id, email, name, password_hash)
SELECT uuidv7(), 'user' || g || '@load.test', 'User ' || g, :'hash' FROM generate_series(1, :n) g
ON CONFLICT (email) DO NOTHING;
-- 1 ou 2 perfis por usuário.
INSERT INTO user_profiles (user_id, profile_id)
SELECT u.id, p.id FROM users u
JOIN profiles p ON p.name = 'profile-' || lpad((1 + abs(hashtext(u.id::text)) % 19)::text, 2, '0')
WHERE u.email LIKE 'user%@load.test'
ON CONFLICT DO NOTHING;
INSERT INTO user_profiles (user_id, profile_id)
SELECT u.id, p.id FROM users u
JOIN profiles p ON p.name = 'profile-' || lpad((1 + abs(hashtext(u.id::text || 'b')) % 19)::text, 2, '0')
WHERE u.email LIKE 'user%@load.test' AND abs(hashtext(u.id::text)) % 2 = 0
ON CONFLICT DO NOTHING;
-- Usuários dos tokens: perfil operador.
INSERT INTO user_profiles (user_id, profile_id)
SELECT u.id, p.id FROM users u, profiles p
WHERE p.name = 'load-operator'
  AND u.email IN (SELECT 'user' || g || '@load.test' FROM generate_series(1, :k) g)
ON CONFLICT DO NOTHING;
-- Exceções em 5% dos usuários (allow/deny em features de access.*).
INSERT INTO user_feature_overrides (user_id, feature_id, effect, reason, created_by, created_by_type)
SELECT u.id, f.id,
       CASE WHEN abs(hashtext(u.id::text || 'e')) % 2 = 0 THEN 'allow' ELSE 'deny' END,
       'carga', u.id, 'tenant'
FROM users u
JOIN features f ON f.code = (ARRAY['access.read', 'access.profiles.manage', 'access.users.manage'])
                            [1 + abs(hashtext(u.id::text || 'f')) % 3]
WHERE u.email LIKE 'user%@load.test' AND abs(hashtext(u.id::text || 'o')) % 100 < 5
ON CONFLICT DO NOTHING;
COMMIT;
ANALYZE;
SQL
}

export_tenant() { # slug id db tokens targets
  local ids
  ids=$(psql_db "$3" -v k="$4" -v t="$5" <<'SQL'
SELECT json_build_object(
  'tokens', (SELECT json_agg(id) FROM users
             WHERE email IN (SELECT 'user' || g || '@load.test' FROM generate_series(1, :k) g)),
  'targets', (SELECT json_agg(id) FROM (SELECT id FROM users WHERE email LIKE 'user%@load.test'
                                        ORDER BY hashtext(id::text) LIMIT :t) s),
  'profiles', (SELECT json_agg(id) FROM profiles WHERE name LIKE 'profile-%'));
SQL
)
  printf '{"slug":"%s","id":"%s",%s' "$1" "$2" "${ids#\{}"
}

echo "== populando tenants"
OUT="$DATA/tenants.json"
echo "[" > "$OUT.tmp"
first=1
# Lista lida antes do laço: `docker compose exec` consumiria o stdin do while.
mapfile -t TENANT_ROWS < <(psql_db "$DB_CONTROL_NAME" -c "SELECT slug, id, db_name FROM tenants WHERE slug LIKE 'load-%' ORDER BY slug" </dev/null)
for row in "${TENANT_ROWS[@]}"; do
  IFS='|' read -r slug id db <<<"$row"
  if [ "$slug" = "load-000" ]; then n=$BIG_USERS; k=$BIG_TOKENS; t=$BIG_TARGETS; else n=$SMALL_USERS; k=$SMALL_TOKENS; t=$SMALL_TARGETS; fi
  have=$(psql_db "$db" -c "SELECT count(*) FROM users WHERE email LIKE 'user%@load.test'" </dev/null)
  if [ "$have" -lt "$n" ]; then
    echo "  $slug: $n usuários"
    populate_tenant "$db" "$n" "$k"
  fi
  [ $first -eq 1 ] || echo "," >> "$OUT.tmp"
  first=0
  export_tenant "$slug" "$id" "$db" "$k" "$t" >> "$OUT.tmp"
done
echo "]" >> "$OUT.tmp"
mv "$OUT.tmp" "$OUT"
OWNER_ID=$(psql_db "$DB_CONTROL_NAME" -c "SELECT id FROM platform_users WHERE role = 'owner'")
printf '{"owner_id":"%s"}\n' "$OWNER_ID" > "$DATA/meta.json"
echo "== pronto: $OUT"
psql_db "$BIG_DB" -c "SELECT 'load-000: ' || count(*) || ' usuários, ' || (SELECT count(*) FROM user_feature_overrides) || ' exceções' FROM users"
