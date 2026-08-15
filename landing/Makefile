PNPM ?= pnpm
LANDING_HOST ?= 127.0.0.1
LANDING_PORT ?= 3000

.PHONY: all help setup dev check build preview

all: help

help:
	@echo "Norte landing"
	@echo "  make setup    - instalar dependencias con el lockfile"
	@echo "  make dev      - servidor de desarrollo"
	@echo "  make check    - comprobar tipos"
	@echo "  make build    - generar el build de producción"
	@echo "  make preview  - construir y servir la versión de producción"
	@echo ""
	@echo "Opcional: LANDING_HOST=0.0.0.0 LANDING_PORT=3010 make dev"

_need_pnpm:
	@command -v $(PNPM) >/dev/null 2>&1 || { \
	  echo "ERROR: no encuentro '$(PNPM)'. Instala pnpm y vuelve a intentarlo."; \
	  exit 1; }

setup: _need_pnpm
	$(PNPM) install --frozen-lockfile

dev: _need_pnpm
	$(PNPM) run dev --hostname $(LANDING_HOST) --port $(LANDING_PORT)

check: _need_pnpm
	$(PNPM) run typecheck

build: _need_pnpm
	$(PNPM) run build

preview: _need_pnpm build
	$(PNPM) run start --hostname $(LANDING_HOST) --port $(LANDING_PORT)
