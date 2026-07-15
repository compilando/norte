#!/usr/bin/env bash
# Bootstrap del entorno de desarrollo de norte. Idempotente: reejecutar es
# seguro (salta lo ya instalado). Instala EXACTAMENTE lo que exige el justfile.
#
#   rustup + toolchain pineado (rust-toolchain.toml → 1.96.1, con rustfmt/
#   clippy/llvm-tools) · just · cargo-nextest · cargo-llvm-cov · cargo-deny
#
# Uso:  make setup     (o directamente:  bash scripts/setup.sh)
set -euo pipefail

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
ok() { printf '\033[1;32m  ✓\033[0m %s\n' "$*"; }
skip() { printf '\033[1;33m  ·\033[0m %s (ya está)\n' "$*"; }

# --- 1) rustup + toolchain pineado ------------------------------------------
if ! command -v rustup >/dev/null 2>&1; then
  info "instalando rustup (respeta rust-toolchain.toml)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain none --no-modify-path
  # shellcheck disable=SC1091
  . "${CARGO_HOME:-$HOME/.cargo}/env"
else
  skip "rustup"
fi

# Asegura el PATH de cargo en esta sesión aunque rustup ya estuviera.
if ! command -v cargo >/dev/null 2>&1; then
  # shellcheck disable=SC1091
  . "${CARGO_HOME:-$HOME/.cargo}/env"
fi

info "materializando el toolchain pineado (rust-toolchain.toml)"
# Cualquier invocación en el repo instala el channel + components pineados.
rustup show >/dev/null
ok "toolchain: $(rustc --version)"

# --- 2) herramientas del justfile -------------------------------------------
# cargo-binstall (si está) baja binarios precompilados: mucho más rápido que
# compilar cada tool. Si no está, caemos a `cargo install` (compila).
inst() { # inst <bin> <crate>
  local bin="$1" crate="$2"
  if command -v "$bin" >/dev/null 2>&1; then
    skip "$bin"
  elif command -v cargo-binstall >/dev/null 2>&1; then
    info "instalando $crate (binstall)"; cargo binstall -y "$crate"; ok "$bin"
  else
    info "instalando $crate (cargo install; compila, tarda)"
    cargo install --locked "$crate"; ok "$bin"
  fi
}

inst just just
inst cargo-nextest cargo-nextest
inst cargo-llvm-cov cargo-llvm-cov
inst cargo-deny cargo-deny

# --- 3) verificación --------------------------------------------------------
info "verificando el setup"
for c in cargo rustc rustfmt just cargo-nextest cargo-deny; do
  command -v "$c" >/dev/null 2>&1 && ok "$c" || { echo "  ✗ falta $c"; exit 1; }
done
rustup component list --installed | grep -q clippy && ok "clippy" || {
  info "añadiendo clippy"; rustup component add clippy; }

echo
ok "listo. Prueba:  make dev   ·   just ci   ·   make test"
echo "   (si 'cargo' no está en el PATH de un shell nuevo, abre otra terminal"
echo "    o ejecuta: . \"\${CARGO_HOME:-\$HOME/.cargo}/env\")"
