# Envoltorio fino sobre `just` (la fuente única de comandos: humanos, Claude
# y CI corren exactamente lo mismo — ver justfile).
#
# ¿Equipo nuevo (sin cargo/just)?  ->  make setup

.PHONY: all setup run dev cli test t ci fmt lint cov docs watch help install uninstall

all: help

# Bootstrap del entorno: rustup + toolchain pineado + just + nextest/llvm-cov/
# deny. Idempotente. NO necesita nada previo salvo curl.
setup:
	bash scripts/setup.sh

help:
	@echo "norte — atajos (delegan en just):"
	@echo "  make setup  - preparar el equipo (rustup, just, nextest, deny…)"
	@echo "  make run    - TUI en release"
	@echo "  make dev    - TUI en debug (iterar)"
	@echo "  make test   - suite completa (nextest + doctests)"
	@echo "  make ci     - lo mismo que CI: lint + test + cobertura + docs"
	@echo "  make fmt    - formatear"
	@echo "  make watch  - tests en cada guardado (exige cargo-watch)"
	@echo "  make install   - instala norte-tui y norte (CLI) en \$$CARGO_HOME/bin"
	@echo "  make uninstall - los desinstala"
	@echo "  just cli ls /tmp          - CLI de humo (args libres via just)"

run:
	just run

dev:
	just dev

test:
	just test

ci:
	just ci

fmt:
	just fmt

lint:
	just lint

cov:
	just cov

docs:
	just docs

watch:
	just watch

install:
	just install

uninstall:
	just uninstall
