# Envoltorio fino sobre `just` (la fuente única de comandos: humanos, Claude
# y CI corren exactamente lo mismo — ver justfile). Instala just con
# `cargo install just` si no lo tienes.

.PHONY: all run dev cli test t ci fmt lint cov docs watch help

all: help

help:
	@echo "norte — atajos (delegan en just):"
	@echo "  make run    - TUI en release"
	@echo "  make dev    - TUI en debug (iterar)"
	@echo "  make test   - suite completa (nextest + doctests)"
	@echo "  make ci     - lo mismo que CI: lint + test + cobertura + docs"
	@echo "  make fmt    - formatear"
	@echo "  make watch  - tests en cada guardado (exige cargo-watch)"
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
