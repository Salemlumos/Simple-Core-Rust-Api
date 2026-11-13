# simple-core — comandos de desenvolvimento, execução e carga.
#
# Modos:
#   make up                         API+worker em container, PostgreSQL em container
#   make db-up dev-init dev-api     API local, PostgreSQL em container (ENV_FILE=.env.local)
#   make up-external                API+worker em container, PostgreSQL externo (ENV_FILE=.env.external)
#   ENV_FILE=.env.external make dev-init dev-api
SHELL := bash
.SHELLFLAGS := -eu -o pipefail -c
.DEFAULT_GOAL := help

COMPOSE_MIN := 2.20.0
COMPOSE = docker compose --env-file $(ENV_FILE)
# Carrega o env-file com a mesma semântica do Compose (valores com espaço, sem expansão).
WITH_ENV = bash scripts/with-env.sh $(ENV_FILE)

# Padrões de ENV_FILE por modo (sobrescreva com ENV_FILE=...).
up down logs reset load-populate load load-scenario: ENV_FILE ?= .env.docker
up-external: ENV_FILE ?= .env.external
db-up dev-init dev-api dev-worker sqlx-prepare sqlx-check test: ENV_FILE ?= .env.local
OPENAPI_OUT ?= openapi.json

.PHONY: help openapi
help:
	@grep -E '^[a-z0-9-]+:.*## ' $(MAKEFILE_LIST) | sed 's/:.*## /\t/' | sort

.PHONY: check-env
check-env:
	@[ -f "$(ENV_FILE)" ] || { echo "Arquivo $(ENV_FILE) não encontrado. Copie $(ENV_FILE).example e preencha os segredos."; exit 1; }

.PHONY: check-compose
check-compose:
	@v=$$(docker compose version --short | sed 's/^v//'); \
	 printf '%s\n%s\n' "$(COMPOSE_MIN)" "$$v" | sort -V -C || { echo "Docker Compose >= $(COMPOSE_MIN) é obrigatório (encontrado $$v)"; exit 1; }

# ------------------------------------------------------------------ containers

up: check-compose check-env ## Sobe PostgreSQL e aplicação em containers (perfis db e app)
	$(COMPOSE) --profile db --profile app up -d --build --wait

up-external: check-compose check-env ## Sobe só a aplicação, usando um PostgreSQL externo (perfil app)
	@grep -qE '^DB_HOST=postgres$$' $(ENV_FILE) && { echo "DB_HOST=postgres aponta para o banco do projeto; use make up"; exit 1; } || true
	$(COMPOSE) --profile app up -d --build --wait

db-up: check-compose check-env ## Sobe só o PostgreSQL do projeto (perfil db)
	$(COMPOSE) --profile db up -d --wait

down: check-compose ## Para e remove os containers (mantém os volumes)
	$(COMPOSE) --profile db --profile app down

logs: check-compose ## Acompanha os logs
	$(COMPOSE) --profile db --profile app logs -f --tail=200

reset: check-compose ## Apaga SOMENTE o volume do PostgreSQL do projeto (pede confirmação)
	@case "$(ENV_FILE)" in *external*) echo "Recusado: reset não é permitido com $(ENV_FILE)."; exit 1;; esac
	@read -r -p "Isto apaga TODOS os dados do PostgreSQL do projeto. Digite 'apagar' para confirmar: " ans; \
	 [ "$$ans" = "apagar" ] || { echo "Cancelado."; exit 1; }
	$(COMPOSE) --profile db --profile app down
	docker volume rm simple-core_pgdata

# ------------------------------------------------------------------ máquina local

dev-init: check-env ## bootstrap + migrate + seed na máquina local
	$(WITH_ENV) cargo run --release --locked -- bootstrap
	$(WITH_ENV) cargo run --release --locked -- migrate
	$(WITH_ENV) cargo run --release --locked -- seed

dev-api: check-env ## api serve na máquina local
	$(WITH_ENV) cargo run --release --locked -- serve

dev-worker: check-env ## api worker na máquina local
	$(WITH_ENV) cargo run --release --locked -- worker

# ------------------------------------------------------------------ qualidade

openapi: ## Grava a especificação OpenAPI em OPENAPI_OUT (padrão openapi.json; sem banco nem env-file)
	cargo run --release --locked -q -- openapi > $(OPENAPI_OUT).tmp && mv $(OPENAPI_OUT).tmp $(OPENAPI_OUT)

sqlx-prepare: check-env ## Regenera .sqlx (banco temporário sqlx_check com os dois schemas)
	ENV_FILE=$(ENV_FILE) bash scripts/sqlx.sh prepare

sqlx-check: check-env ## Falha se .sqlx estiver desatualizado
	ENV_FILE=$(ENV_FILE) bash scripts/sqlx.sh check

.PHONY: fmt lint test
fmt:
	cargo fmt --check

lint:
	SQLX_OFFLINE=true cargo clippy --all-targets --locked -- -D warnings

test: check-env ## Testes unitários e de integração (PostgreSQL real; requer make db-up)
	SQLX_OFFLINE=true $(WITH_ENV) cargo test --locked -- --test-threads=$${TEST_THREADS:-4}

# ------------------------------------------------------------------ carga

load-populate: check-env ## Cria os 100 tenants, usuários, perfis e exceções da carga
	ENV_FILE=$(ENV_FILE) bash load/populate.sh

load: check-env ## Roda todos os cenários de carga (API e banco já no ar)
	ENV_FILE=$(ENV_FILE) bash load/run.sh all

load-scenario: check-env ## Roda um cenário: make load-scenario S=get_by_id
	ENV_FILE=$(ENV_FILE) bash load/run.sh $(S)
