#!/usr/bin/env bash
# Humo de los artefactos de una build de la base, en contenedores limpios,
# según `matrix.txt` (ADR 0112).
#
#   scripts/baseline/smoke.sh DIR [artefacto]
#   scripts/baseline/smoke.sh --one DIR REV ARTEFACTO IMAGEN
#
# Cada ejecución deja su log en DIR/smoke/ y, si pasa, una línea
# `ok artefacto imagen` en DIR/SMOKE. Un fallo no para a los demás: al final
# se sabe TODO lo que falla. NORTE_SMOKE_JOBS ejecuciones a la vez (4).
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"
MATRIX="$RAIZ/scripts/baseline/matrix.txt"

rojo() { printf '\033[31m%s\033[0m\n' "$*" >&2; }
verde() { printf '\033[32m%s\033[0m\n' "$*"; }

una() {
  local dir="$1" rev="$2" artifact="$3" image="$4" sub log
  case "$artifact" in
    tarball | installer) sub=dist ;;
    deb | rpm | appimage) sub=gui ;;
    *)
      rojo "artefacto desconocido: $artifact"
      return 2
      ;;
  esac
  mkdir -p "$dir/smoke"
  log="$dir/smoke/$artifact-$(image_slug "$image").log"
  if docker run --rm -i \
    -v "$dir/$sub:/a:ro" \
    -e ARTIFACT="$artifact" \
    -e REV="$rev" \
    -e DEBIAN_FRONTEND=noninteractive \
    "$image" bash -s <"$RAIZ/scripts/baseline/smoke-inside.sh" >"$log" 2>&1; then
    echo "ok $artifact $image" >>"$dir/SMOKE"
    verde "ok     $artifact $(image_slug "$image")"
  else
    rojo "FALLA  $artifact $(image_slug "$image") — $log"
    return 1
  fi
}

if [ "${1:-}" = "--one" ]; then
  shift
  una "$@"
  exit
fi

DIR="$(realpath "${1:?uso: smoke.sh DIR [artefacto]}")"
SOLO="${2:-}"
[ -f "$DIR/MANIFEST" ] || { rojo "no hay $DIR/MANIFEST: ¿es una salida de build.sh?"; exit 1; }
REV="$(awk '$1 == "revision" { print $2 }' "$DIR/MANIFEST")"

lineas="$(grep -vE '^[[:space:]]*(#|$)' "$MATRIX" | awk -v solo="$SOLO" 'solo == "" || $1 == solo')"
[ -n "$lineas" ] || { rojo "ninguna línea de la matriz para «$SOLO»"; exit 1; }
[ -n "$SOLO" ] || : >"$DIR/SMOKE"

# `xargs` sale 123 si alguna ejecución falló, y `set -e` lo propaga.
printf '%s\n' "$lineas" |
  xargs -P "${NORTE_SMOKE_JOBS:-4}" -L 1 "$0" --one "$DIR" "$REV"
