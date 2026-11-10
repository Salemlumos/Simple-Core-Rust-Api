#!/usr/bin/env bash
# Executa cenários de carga com k6 (em container, CPUs 8-15), API em 0-3 e
# PostgreSQL em 4-7 (load/compose.load.yaml). Registra RPS, p50/p95/p99,
# erros, CPU, memória, conexões abertas e taxa de acerto dos caches.
#
#   bash load/run.sh all              todos os cenários
#   bash load/run.sh get_by_id        um cenário
#   RATE=20000 bash load/run.sh get_by_id
#
# Pré-requisitos: make up + make load-populate.
set -euo pipefail
cd "$(dirname "$0")/.."

ENV_FILE="${ENV_FILE:-.env.docker}"
if [ -z "${__ENV_LOADED:-}" ]; then
  exec env __ENV_LOADED=1 bash scripts/with-env.sh "$ENV_FILE" bash "$0" "$@"
fi
export MSYS_NO_PATHCONV=1

WARMUP_S="${WARMUP_S:-30}"
DURATION_S="${DURATION_S:-180}"
RUN_ID="${RUN_ID:-$(date +%Y%m%d-%H%M%S)}"
OUT="load/results/$RUN_ID"
mkdir -p "$OUT"
HOSTDIR="$(pwd -W 2>/dev/null || pwd)"
COMPOSE=(docker compose --env-file "$ENV_FILE" -f compose.yaml -f load/compose.load.yaml --profile db --profile app)
NET="${COMPOSE_PROJECT_NAME:-simple-core}_default"
API_CTR="${COMPOSE_PROJECT_NAME:-simple-core}-api-1"
PG_CTR="${COMPOSE_PROJECT_NAME:-simple-core}-postgres-1"
METRICS="http://127.0.0.1:${METRICS_PUBLISH_PORT:-9090}/metrics"

# Taxas padrão (req/s) por cenário; sobrescreva com RATE.
declare -A RATES=(
  [get_by_id]=15000 [list]=4000 [mix_single]=9000 [mix_all]=9000 [pool_churn]=8000
  [noisy_neighbor]=1500 [login]=150 [create]=150 [read_with_invalidations]=10000
  [read_with_provisioning]=10000
)
ALL=(get_by_id list mix_single mix_all pool_churn noisy_neighbor login create read_with_invalidations read_with_provisioning)

psql_db() { "${COMPOSE[@]}" exec -T postgres psql -X -q -At -v ON_ERROR_STOP=1 -U "$DB_ADMIN_USER" -d "$@"; }

api_up() { # aplica overrides de ambiente (ex.: MAX_OPEN_TENANT_POOLS) e espera ficar saudável
  "${COMPOSE[@]}" up -d --no-deps --wait api worker postgres >/dev/null
}

# Vítimas para DELETE: usuários novos por tenant, na proporção do tráfego.
make_victims() { # cenário rate
  local scen="$1" rate="$2" total file="$OUT/victims-$1.json"
  total=$(( rate * (WARMUP_S + DURATION_S) * 2 / 100 + 2000 ))
  local i slugs
  mapfile -t slugs < <(grep -o '"slug":"[^"]*"' load/data/tenants.json | cut -d'"' -f4)
  echo "[" > "$file"
  local first=1
  for i in "${!slugs[@]}"; do
    local n
    if [ "$scen" = "mix_single" ]; then
      [ "$i" -eq 0 ] && n=$total || continue
    else
      # Zipf s=1 sobre 100 tenants: H_100 ≈ 5.187
      n=$(awk -v t="$total" -v i="$i" 'BEGIN { printf "%d", 1.2 * t / (i + 1) / 5.187 + 5 }')
    fi
    local db ids
    db=$(psql_db "$DB_CONTROL_NAME" -c "SELECT db_name FROM tenants WHERE slug = '${slugs[$i]}'")
    ids=$(psql_db "$db" -v n="$n" -v run="$RUN_ID" -v hash='x' <<'SQL'
WITH v AS (
  INSERT INTO users (id, email, name, password_hash)
  SELECT uuidv7(), 'victim-' || :'run' || '-' || g || '@load.test', 'Victim', 'x' FROM generate_series(1, :n) g
  ON CONFLICT (email) DO NOTHING
  RETURNING id
) SELECT string_agg(id::text, ' ') FROM v;
SQL
)
    for id in $ids; do
      [ $first -eq 1 ] || printf ',' >> "$file"
      first=0
      printf '{"t":%d,"id":"%s"}' "$i" "$id" >> "$file"
    done
  done
  echo "]" >> "$file"
  echo "/load/results/$RUN_ID/victims-$scen.json"
}

scrape_caches() { curl -s "$METRICS" | grep '^cache_requests_total' || true; }

cache_rates() { # antes depois
  awk 'NR==FNR { before[$1]=$2; next } { after[$1]=$2 }
       END {
         for (k in after) {
           split(k, a, "\""); cache=a[2]; res=a[4];
           d = after[k] - (k in before ? before[k] : 0);
           tot[cache] += d; if (res == "hit") hit[cache] += d;
         }
         out = "";
         for (c in tot) if (tot[c] > 0) out = out sprintf("%s %.2f%% ", c, 100 * hit[c] / tot[c]);
         print out;
       }' "$1" "$2"
}

sample_stats() { # arquivo — amostra CPU/memória/conexões a cada 5 s até ser morto
  while true; do
    local s c
    s=$(docker stats --no-stream --format '{{.Name}};{{.CPUPerc}};{{.MemUsage}}' "$API_CTR" "$PG_CTR" 2>/dev/null | tr '\n' '|')
    c=$(psql_db postgres -c "SELECT count(*) FILTER (WHERE usename = '$DB_RUNTIME_USER'), count(*) FILTER (WHERE usename = '$DB_RUNTIME_USER' AND state = 'active') FROM pg_stat_activity" 2>/dev/null | tr '|' ';')
    echo "$s$c" >> "$1"
    sleep 5
  done
}

summarize_stats() {
  awk -F'|' -v api="$API_CTR" -v pg="$PG_CTR" '
    function num(x) { gsub(/[^0-9.]/, "", x); return x + 0 }
    function mib(x) { v = num(x); if (x ~ /GiB/) v *= 1024; if (x ~ /KiB/) v /= 1024; return v }
    {
      for (i = 1; i <= NF; i++) {
        n = split($i, f, ";");
        if (f[1] == api) { ac += num(f[2]); an++; if (num(f[2]) > am) am = num(f[2]); split(f[3], m, "/"); if (mib(m[1]) > amem) amem = mib(m[1]) }
        else if (f[1] == pg) { pc += num(f[2]); pn++; if (num(f[2]) > pm) pm = num(f[2]); split(f[3], m, "/"); if (mib(m[1]) > pmem) pmem = mib(m[1]) }
        else if (n == 2 && f[1] ~ /^[0-9]+$/) { co += f[1]; cn++; if (f[1] > cm) cm = f[1]; if (f[2] > am2) am2 = f[2] }
      }
    }
    END {
      printf "%.0f%% / %.0f%% | %.0f MiB | %.0f%% / %.0f%% | %.0f MiB | %.0f / %d (ativas máx %d) |",
        (an ? ac / an : 0), am, amem, (pn ? pc / pn : 0), pm, pmem, (cn ? co / cn : 0), cm, am2
    }' "$1"
}

run_scenario() {
  local scen="$1" rate="${RATE:-${RATES[$1]}}" victims=""
  echo "== $scen (rate=$rate/s, aquecimento ${WARMUP_S}s, medição ${DURATION_S}s)"
  case "$scen" in
    pool_churn) MAX_OPEN_TENANT_POOLS="${CHURN_POOLS:-20}" TENANT_POOL_MAX_CONNECTIONS="${CHURN_POOL_CONNS:-5}" api_up ;;
    *) api_up ;;
  esac
  case "$scen" in mix_single | mix_all) victims=$(make_victims "$scen" "$rate") ;; esac

  local before="$OUT/$scen.cache.before" after="$OUT/$scen.cache.after" stats="$OUT/$scen.stats"
  : > "$stats"
  ( sleep "$WARMUP_S"; scrape_caches > "$before"; sample_stats "$stats" ) &
  local sampler=$!
  docker run --rm --network "$NET" --cpuset-cpus "${K6_CPUS:-8-15}" \
    -v "$HOSTDIR/load:/load" \
    -e SCENARIO="$scen" -e RATE="$rate" -e RUN_ID="$RUN_ID" -e VICTIMS="$victims" \
    -e WARMUP="${WARMUP_S}s" -e DURATION="${DURATION_S}s" -e VUS="${VUS:-400}" \
    -e NOISY_RATE="${NOISY_RATE:-6000}" \
    -e JWT_TENANT_SECRET="$JWT_TENANT_SECRET" -e JWT_PLATFORM_SECRET="$JWT_PLATFORM_SECRET" \
    -e JWT_ISSUER="${JWT_ISSUER:-simple-core}" \
    grafana/k6:latest run --quiet --no-color /load/k6/main.js 2>&1 | grep -vE '^\s*$' | tail -6 || true
  kill "$sampler" 2>/dev/null || true
  wait "$sampler" 2>/dev/null || true
  scrape_caches > "$after"
  local caches res
  caches=$(cache_rates "$before" "$after")
  res=$(summarize_stats "$stats")
  while IFS= read -r line; do
    [ -n "$line" ] && echo "${line} ${res} ${caches} |" >> "$OUT/summary.md"
  done < "$OUT/$scen.md"
  tail -n 2 "$OUT/summary.md"
  [ "$scen" = "pool_churn" ] && api_up
  return 0
}

if [ ! -f "$OUT/summary.md" ]; then
  {
    echo "| Cenário | Taxa pedida | RPS | p50 ms | p95 ms | p99 ms | Erros | CPU API méd/máx | Mem API máx | CPU PG méd/máx | Mem PG máx | Conexões runtime méd/máx | Acerto dos caches |"
    echo "|---|---|---|---|---|---|---|---|---|---|---|---|---|"
  } > "$OUT/summary.md"
fi

target="${1:-all}"
if [ "$target" = "all" ]; then
  for s in "${ALL[@]}"; do run_scenario "$s"; done
else
  run_scenario "$target"
fi
echo "Resultados: $OUT/summary.md"
