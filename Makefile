# Envoltorio fino sobre `just` (la fuente única de comandos: humanos, Claude
# y CI corren exactamente lo mismo — ver justfile).
#
# ¿Equipo nuevo (sin cargo/just)?  ->  make setup

# cargo/just/nextest viven en $CARGO_HOME/bin (~/.cargo/bin por defecto). Se
# fuerza en el PATH de las recipes para que `make <lo-que-sea>` funcione JUSTO
# tras `make setup`, sin reiniciar el shell (el instalador de rustup no toca el
# PATH del shell en curso).
CARGO_HOME ?= $(HOME)/.cargo
export PATH := $(CARGO_HOME)/bin:$(PATH)

.PHONY: all setup run dev gui cli test t ci fmt lint cov docs watch help install uninstall

all: help

# Guarda: si tras el PATH ni cargo ni just están, el problema es el setup.
_need_just:
	@command -v just >/dev/null 2>&1 || { \
	  echo "ERROR: no encuentro 'just'. Corre 'make setup' (y si ya lo hiciste,"; \
	  echo "       abre una terminal nueva o revisa que exista $(CARGO_HOME)/bin/just)."; \
	  exit 1; }

# Bootstrap del entorno: rustup + toolchain pineado + just + nextest/llvm-cov/
# deny. Idempotente. NO necesita nada previo salvo curl.
setup:
	bash scripts/setup.sh

help:
	@echo "norte — atajos (delegan en just):"
	@echo "  make setup  - preparar el equipo (rustup, just, nextest, deny…)"
	@echo "  make run    - TUI en release"
	@echo "  make dev    - TUI en debug (iterar)"
	@echo "  make gui    - GUI GPUI (excluida del workspace; exige 'norte daemon run')"
	@echo "  make test   - suite completa (nextest + doctests)"
	@echo "  make ci     - lo mismo que CI: lint + test + cobertura + docs"
	@echo "  make fmt    - formatear"
	@echo "  make watch  - tests en cada guardado (exige cargo-watch)"
	@echo "  make install   - instala norte-tui y norte (CLI) en \$$CARGO_HOME/bin"
	@echo "  make uninstall - los desinstala"
	@echo "  just cli ls /tmp          - CLI de humo (args libres via just)"

run: _need_just
	just run

dev: _need_just
	just dev

gui: _need_just
	just gui

test: _need_just
	just test

ci: _need_just
	just ci

fmt: _need_just
	just fmt

lint: _need_just
	just lint

cov: _need_just
	just cov

docs: _need_just
	just docs

watch: _need_just
	just watch

install: _need_just
	just install

uninstall:
	just uninstall
