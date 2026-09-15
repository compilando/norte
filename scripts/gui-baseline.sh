#!/usr/bin/env bash
# Construye el paquete de la ventana sobre una base VIEJA (tarea 7.1).
#
# Un binario enlazado contra la glibc de la máquina que lo compila exige esa
# glibc o una más nueva. `just gui-package` compila en la máquina de
# referencia, que va al día, así que su `.deb` no arranca en una distribución
# de hace dos años aunque ésta tenga WebKitGTK 4.1. Esto compila DENTRO de la
# base más vieja que tiene WebKitGTK 4.1 —Ubuntu 22.04, glibc 2.35— y deja el
# resultado donde `gui-smoke` lo puede instalar en otras bases.
#
#   scripts/gui-baseline.sh [imagen]
#
# Qué se construye es el COMMIT, no el árbol: el código entra por `git
# archive HEAD`. Así el contenedor no pisa el `target/` ni el `node_modules`
# del host, y lo que se prueba es exactamente lo que se etiquetaría.
#
# Los tres volúmenes con nombre (`norte-baseline-*`) guardan rustup, el
# registro de cargo y el `target/` de la base entre corridas: la primera es
# una compilación en frío; las siguientes, no. `docker volume rm` los borra.
#
# Salida: `target/baseline/<imagen>/` con el `.deb`, el `.rpm`, el AppImage y
# un `.sha256` por fichero en el formato de `dist` (`<hash> *<fichero>`), más
# `glibc.txt` con la versión de glibc más alta que pide cada binario.
set -euo pipefail

IMAGEN="${1:-ubuntu:22.04}"
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SLUG="${IMAGEN//[:\/]/-}"
SALIDA="$RAIZ/target/baseline/$SLUG"

rojo() { printf '\033[31m%s\033[0m\n' "$*" >&2; }
verde() { printf '\033[32m%s\033[0m\n' "$*"; }

command -v docker >/dev/null || { rojo "hace falta Docker"; exit 1; }

if [[ -n "$(git -C "$RAIZ" status --porcelain --untracked-files=no)" ]]; then
  rojo "aviso: hay cambios sin commitear; se construye HEAD, no el árbol"
fi

TOOLCHAIN="$(grep -E '^channel' "$RAIZ/rust-toolchain.toml" | cut -d'"' -f2)"
# Las MISMAS features que `gui-package`: el paquete de la base vieja tiene que
# ser el mismo producto, no otro.
FEATURES="$(cd "$RAIZ" && just --evaluate features)"

rm -rf "$SALIDA"
mkdir -p "$SALIDA"
echo "base: $IMAGEN · toolchain $TOOLCHAIN · commit $(git -C "$RAIZ" rev-parse --short HEAD)"

DENTRO="$(cat <<'DENTRO'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
export CARGO_HOME=/cargo RUSTUP_HOME=/rustup CARGO_TARGET_DIR=/target
export PATH="/cargo/bin:/node/bin:$PATH"

# El código llega por stdin y es lo PRIMERO que se lee.
mkdir -p /src
tar -x -C /src

echo "--- dependencias del sistema"
apt-get update -qq
# La lista de `.github/workflows/gui.yml`, más lo que el host da por supuesto:
# `lld` porque `.cargo/config.toml` fuerza `-fuse-ld=lld`, `cmake`/`clang`
# para aws-lc-sys, y `file`/`xz-utils` para el bundler.
apt-get install -y -qq --no-install-recommends \
  build-essential clang cmake lld pkg-config curl ca-certificates git file xz-utils \
  libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev librsvg2-dev patchelf >/dev/null

echo "--- Node 22"
# La rama de `gui.yml`. La suma se comprueba contra la que publica nodejs.org.
if [[ ! -x /node/bin/node ]]; then
  base=https://nodejs.org/dist/latest-v22.x
  curl -fsSL "$base/SHASUMS256.txt" -o /tmp/sumas
  tarball="$(grep -oE 'node-v22\.[0-9]+\.[0-9]+-linux-x64\.tar\.xz' /tmp/sumas | head -1)"
  curl -fsSL "$base/$tarball" -o "/tmp/$tarball"
  (cd /tmp && grep " $tarball\$" sumas | sha256sum -c -)
  mkdir -p /node
  tar -xJf "/tmp/$tarball" -C /node --strip-components=1
fi
node --version

echo "--- Rust $TOOLCHAIN"
if ! command -v rustup >/dev/null; then
  curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal \
    --default-toolchain "$TOOLCHAIN" >/dev/null
fi
rustup toolchain install "$TOOLCHAIN" --profile minimal >/dev/null
cd /src
rustc --version

echo "--- webview"
(cd crates/norte-gui-tauri/ui && npm ci --no-audit --no-fund && npm run build)

echo "--- daemon y terminal"
# `$FEATURES` sin comillas a propósito: son varias palabras `--features x`.
# shellcheck disable=SC2086
cargo build --release -p norte-cli -p norte-tui $FEATURES
triple="$(rustc -vV | grep '^host:' | cut -d' ' -f2)"
mkdir -p crates/norte-gui-tauri/binaries
for b in norte ntc; do
  cp "/target/release/$b" "crates/norte-gui-tauri/binaries/$b-$triple"
done

echo "--- paquete"
# Un bundle de una corrida anterior con otra versión se colaría en la salida.
rm -rf /target/release/bundle
# NO_STRIP: el `strip` de linuxdeploy no entiende `.relr.dyn` (#256).
# APPIMAGE_EXTRACT_AND_RUN: linuxdeploy es un AppImage, y un contenedor no
# tiene FUSE.
(cd crates/norte-gui-tauri && NO_STRIP=1 APPIMAGE_EXTRACT_AND_RUN=1 \
  ./ui/node_modules/.bin/tauri build)

find /target/release/bundle -maxdepth 2 -type f \
  \( -name '*.deb' -o -name '*.rpm' -o -name '*.AppImage' \) -exec cp {} /out/ \;
cd /out
for f in *.deb *.rpm *.AppImage; do
  sha256sum --binary "$f" >"$f.sha256"
done

# La glibc más alta que pide cada binario: el número que decide en qué
# distribuciones arranca el paquete.
for b in norte-gui norte ntc; do
  v="$(objdump -T "/target/release/$b" | grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' | sort -Vu | tail -1)"
  echo "$b $v"
done | tee /out/glibc.txt

chown -R "$HOST_UID:$HOST_GID" /out
DENTRO
)"

git -C "$RAIZ" archive --format=tar HEAD | docker run --rm -i \
  -v norte-baseline-rustup:/rustup \
  -v norte-baseline-cargo:/cargo \
  -v norte-baseline-target:/target \
  -v norte-baseline-node:/node \
  -v "$SALIDA:/out" \
  -e TOOLCHAIN="$TOOLCHAIN" \
  -e FEATURES="$FEATURES" \
  -e NORTE_REVISION="$(git -C "$RAIZ" describe --tags --always --long HEAD)" \
  -e HOST_UID="$(id -u)" \
  -e HOST_GID="$(id -g)" \
  "$IMAGEN" bash -c "$DENTRO"

ls -1 "$SALIDA"
verde "paquete de la base vieja: $SALIDA"
