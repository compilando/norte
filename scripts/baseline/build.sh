#!/usr/bin/env bash
# Construye TODO lo que se publicaría de una referencia —tarballs e
# instaladores de `dist`, y el deb/rpm/AppImage de la ventana— dentro de la
# imagen de construcción, y deja un MANIFEST que dice qué glibc pide cada
# binario y qué revisión dice ser (ADR 0112).
#
#   scripts/baseline/build.sh [ref]      # por defecto HEAD
#
# El repo entra por un clon DENTRO del contenedor desde el `.git` montado en
# solo lectura: la revisión (`git describe`) sale del ref pedido y no de
# HEAD, y `dist` puede hacer su `source.tar.gz`. El `target/` del host no se
# toca.
#
# Todo el progreso va a stderr; stdout es UNA línea, el directorio de salida.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

REF="${1:-HEAD}"
FLOOR="${NORTE_GLIBC_FLOOR:-2.35}"

for c in docker git bsdtar objdump; do
  command -v "$c" >/dev/null || { echo "hace falta $c" >&2; exit 1; }
done

commit="$(git -C "$RAIZ" rev-parse --verify "$REF^{commit}")"
revision="$(git -C "$RAIZ" describe --tags --always --long "$commit")"
gitdir="$(git -C "$RAIZ" rev-parse --path-format=absolute --git-common-dir)"
SALIDA="$RAIZ/target/baseline/$revision"
imagen="$("$RAIZ/scripts/baseline/image.sh")"

echo "ref $REF · commit ${commit:0:12} · revisión $revision · $imagen" >&2
rm -rf "$SALIDA"
mkdir -p "$SALIDA"

# shellcheck disable=SC2016  # se expande DENTRO del contenedor
DENTRO='
set -euo pipefail
export CARGO_TARGET_DIR=/target
git clone -q /repo.git /src
cd /src
git checkout -q "$COMMIT"
triple="$(rustc -vV | grep "^host:" | cut -d" " -f2)"
rm -rf /target/distrib /src/target/distrib /target/release/bundle

echo "--- dist: norte y ntc"
dist build --artifacts=local --target="$triple"
dist build --artifacts=global --target="$triple"

echo "--- webview"
(cd crates/norte-gui-tauri/ui && npm ci --no-audit --no-fund && npm run build)

echo "--- paquete de la ventana, con los MISMOS norte y ntc que los tarballs"
mkdir -p crates/norte-gui-tauri/binaries
for b in norte ntc; do
  src="$(find /target -type f -perm -u+x -path "*/dist/$b" -print -quit)"
  [ -n "$src" ] || { echo "dist no dejó el binario $b bajo /target" >&2; exit 1; }
  cp "$src" "crates/norte-gui-tauri/binaries/$b-$triple"
done
(cd crates/norte-gui-tauri && NO_STRIP=1 APPIMAGE_EXTRACT_AND_RUN=1 ./ui/node_modules/.bin/tauri build)

mkdir -p /out/dist /out/gui
distrib=/target/distrib
[ -d "$distrib" ] || distrib=/src/target/distrib
find "$distrib" -maxdepth 1 -type f -exec cp {} /out/dist/ \;
find /target/release/bundle -maxdepth 2 -type f \( -name "*.deb" -o -name "*.rpm" -o -name "*.AppImage" \) -exec cp {} /out/gui/ \;
chown -R "$HOST_UID:$HOST_GID" /out
'

docker run --rm \
  -v "$gitdir:/repo.git:ro" \
  -v norte-baseline-registry:/opt/cargo/registry \
  -v norte-baseline-target:/target \
  -v "$SALIDA:/out" \
  -e COMMIT="$commit" \
  -e HOST_UID="$(id -u)" \
  -e HOST_GID="$(id -g)" \
  "$imagen" bash -c "$DENTRO" >&2

# --- MANIFEST: se abre lo que se publicaría y se mira DENTRO -------------
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/tar" "$tmp/deb"
for t in "$SALIDA"/dist/*.tar.xz; do tar -xf "$t" -C "$tmp/tar"; done
deb="$(find "$SALIDA/gui" -name '*.deb' -print -quit)"
[ -n "$deb" ] || { echo "la build no dejó .deb" >&2; exit 1; }
bsdtar -xOf "$deb" 'data.tar.*' | bsdtar -x -C "$tmp/deb"

{
  echo "ref $REF"
  echo "commit $commit"
  echo "revision $revision"
  echo "glibc-floor $FLOOR"
  echo "builder $imagen"
  while IFS= read -r bin; do
    rel="${bin#"$tmp"/}"
    v="$(objdump -T "$bin" | glibc_max)"
    echo "glibc $rel ${v:-none}"
    case "${bin##*/}" in
      norte | ntc) echo "version $rel $("$bin" --version)" ;;
    esac
  done < <(find "$tmp" -type f \( -name norte -o -name ntc -o -name norte-gui \) | LC_ALL=C sort)
} >"$SALIDA/MANIFEST"

manifest_sums "$SALIDA"

problemas="$(manifest_problems "$SALIDA/MANIFEST")"
if [ -n "$problemas" ]; then
  printf 'la build NO vale:\n%s\n' "$problemas" >&2
  exit 1
fi
printf '%s\n' "$SALIDA"
