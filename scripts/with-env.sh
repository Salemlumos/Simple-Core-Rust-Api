#!/usr/bin/env bash
# Uso: scripts/with-env.sh ARQUIVO comando [args...]
# Exporta as variáveis do arquivo .env (mesmo formato do --env-file do Compose:
# KEY=valor, sem expansão de shell; aspas externas opcionais) e executa o
# comando. Variáveis já definidas no ambiente têm precedência.
set -euo pipefail
file="$1"
shift
[ -f "$file" ] || { echo "arquivo $file não encontrado" >&2; exit 1; }
while IFS= read -r line || [ -n "$line" ]; do
  line="${line%$'\r'}"
  case "$line" in '' | '#'*) continue ;; esac
  key="${line%%=*}"
  val="${line#*=}"
  [[ "$key" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || continue
  if [[ "$val" =~ ^\"(.*)\"$ ]] || [[ "$val" =~ ^\'(.*)\'$ ]]; then
    val="${BASH_REMATCH[1]}"
  fi
  if [ -z "${!key+x}" ]; then
    export "$key=$val"
  fi
done <"$file"
exec "$@"
