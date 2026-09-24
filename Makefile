# Thin wrapper over `just` (the single source of commands: humans, Claude
# and CI all run exactly the same thing — see justfile).
#
# New machine (no cargo/just)?  ->  make setup

# cargo/just/nextest live in $CARGO_HOME/bin (~/.cargo/bin by default). It is
# forced onto the recipes' PATH so `make <whatever>` works RIGHT after
# `make setup`, without restarting the shell (rustup's installer does not
# touch the current shell's PATH).
CARGO_HOME ?= $(HOME)/.cargo
export PATH := $(CARGO_HOME)/bin:$(PATH)

.PHONY: all setup link link-gui link-all unlink run dev gui cli test t ci fmt lint cov docs watch help install uninstall

all: help

# Guard: if after the PATH neither cargo nor just are there, the problem is
# the setup.
_need_just:
	@command -v just >/dev/null 2>&1 || { \
	  echo "ERROR: cannot find 'just'. Run 'make setup' (and if you already did,"; \
	  echo "       open a new terminal or check that $(CARGO_HOME)/bin/just exists)."; \
	  exit 1; }

# Environment bootstrap: rustup + pinned toolchain + just + nextest/llvm-cov/
# deny. Idempotent. Needs NOTHING beforehand except curl.
setup:
	bash scripts/setup.sh

# Puts `ntc`, `norte` and `ntc-gui` on the PATH pointing at THIS tree. This is
# what is run ONCE after `make setup`; from then on any build updates them on
# its own, because they are symlinks to this tree's `target/` and not copies.
#
# `make setup` prepares the TOOLCHAIN; this prepares the COMMANDS. Two
# different steps, in that order.
link-all: _need_just
	just link-all

# The two halves on their own, in case you only want one: `link` is ntc +
# norte (without touching WebKitGTK or npm), `link-gui` is the window.
link: _need_just
	just link

link-gui: _need_just
	just link-gui

# `make link-all`'s reverse. It only removes the links that point at this
# tree; another worktree's stay.
unlink: _need_just
	just unlink

help:
	@echo "norte — shortcuts (delegate to just):"
	@echo "  make setup    - prepare the machine (rustup, just, nextest, deny…)"
	@echo "  make link-all - put ntc, norte and ntc-gui on the PATH (after setup)"
	@echo "  make unlink   - remove them"
	@echo "  make run      - TUI in release"
	@echo "  make dev      - TUI in debug (iterate)"
	@echo "  make gui      - Tauri window (requires 'norte daemon run')"
	@echo "  make test     - full suite (nextest + doctests)"
	@echo "  make ci       - same as CI: lint + test + coverage + docs"
	@echo "  make fmt      - format"
	@echo "  make watch    - tests on every save (requires cargo-watch)"
	@echo "  make install  - installs norte-tui and norte (CLI) into \$$CARGO_HOME/bin"
	@echo "  make uninstall - uninstalls them"
	@echo "  just cli ls /tmp          - smoke CLI (free args via just)"

run: _need_just
	just run

dev: _need_just
	just dev

gui: _need_just
	just gui-run

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
