#!/usr/bin/env bash
# Bootstrap for norte's development environment. Idempotent: re-running is
# safe (skips what is already installed). Installs EXACTLY what the justfile
# requires.
#
#   rustup + pinned toolchain (rust-toolchain.toml → 1.96.1, with rustfmt/
#   clippy/llvm-tools) · just · cargo-nextest · cargo-llvm-cov · cargo-deny
#
# Usage:  make setup     (or directly:  bash scripts/setup.sh)
set -euo pipefail

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok() { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
skip() { printf '\033[1;33m  ·\033[0m %s (already there)\n' "$*"; }

# --- 1) rustup + pinned toolchain -------------------------------------------
if ! command -v rustup >/dev/null 2>&1; then
  info "installing rustup (respects rust-toolchain.toml)"
  # WITHOUT --no-modify-path: let rustup add ~/.cargo/bin to the shell's PATH
  # (profiles), so NEW terminals have cargo/just without any tricks.
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain none
  # shellcheck disable=SC1091
  . "${CARGO_HOME:-$HOME/.cargo}/env"
else
  skip "rustup"
fi

# Ensures cargo's PATH in this session even if rustup was already there.
if ! command -v cargo >/dev/null 2>&1; then
  # shellcheck disable=SC1091
  . "${CARGO_HOME:-$HOME/.cargo}/env"
fi

info "materializing the pinned toolchain (rust-toolchain.toml)"
# Any invocation in the repo installs the pinned channel + components.
rustup show >/dev/null
ok "toolchain: $(rustc --version)"

# --- 2) justfile tools -------------------------------------------------------
# cargo-binstall (if present) downloads precompiled binaries: much faster than
# compiling each tool. If absent, falls back to `cargo install` (compiles).
inst() { # inst <bin> <crate>
  local bin="$1" crate="$2"
  if command -v "$bin" >/dev/null 2>&1; then
    skip "$bin"
  elif command -v cargo-binstall >/dev/null 2>&1; then
    info "installing $crate (binstall)"; cargo binstall -y "$crate"; ok "$bin"
  else
    info "installing $crate (cargo install; compiles, takes a while)"
    cargo install --locked "$crate"; ok "$bin"
  fi
}

inst just just
inst cargo-nextest cargo-nextest
inst cargo-llvm-cov cargo-llvm-cov
inst cargo-deny cargo-deny

# --- 3) verification ---------------------------------------------------------
info "verifying the setup"
for c in cargo rustc rustfmt just cargo-nextest cargo-deny; do
  command -v "$c" >/dev/null 2>&1 && ok "$c" || { echo "  ✗ missing $c"; exit 1; }
done
rustup component list --installed | grep -q clippy && ok "clippy" || {
  info "adding clippy"; rustup component add clippy; }

echo
ok "done. Try:  make dev   ·   just ci   ·   make test"
echo "   (if 'cargo' is not on a new shell's PATH, open another terminal"
echo "    or run: . \"\${CARGO_HOME:-\$HOME/.cargo}/env\")"
