# ============================================================================
# RSMC engine developer commands
# ============================================================================
# Run `make` or `make help` to list targets.

.PHONY: help dev dev-fresh reset-db down logs check-migrations freeze-migrations

help:              ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
	  awk 'BEGIN{FS=":.*?## "}{printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

# ---- Local stack (Docker Compose) -----------------------------------------

dev:               ## Start the stack, KEEPING existing data
	docker compose up

dev-fresh:         ## WIPE the dev database + Redis, then start clean (DEV ONLY)
	docker compose down -v
	docker compose up

reset-db:          ## Wipe ONLY the Postgres volume, leave everything else (DEV ONLY)
	docker compose stop app || true
	docker compose rm -sfv postgres
	docker compose up -d postgres
	docker compose up app

down:              ## Stop the stack, KEEP data
	docker compose down

logs:              ## Tail the engine logs
	docker compose logs -f app

# ---- Migration safety ------------------------------------------------------

check-migrations:  ## Fail if any already-frozen migration was modified
	bash scripts/check-migrations.sh

freeze-migrations: ## Re-baseline the frozen migration checksums (use with care)
	bash scripts/freeze-migrations.sh
