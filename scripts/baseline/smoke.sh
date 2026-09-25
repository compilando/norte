#!/usr/bin/env bash
# Smoke test of a baseline build's artifacts, in clean containers, according
# to `matrix.txt` (ADR 0112).
#
#   scripts/baseline/smoke.sh DIR [artefacto]
#   scripts/baseline/smoke.sh --one DIR REV ARTEFACTO IMAGEN
#
# Each run leaves its log in DIR/smoke/ and, if it passes, an
# `ok artefacto imagen` line in DIR/SMOKE. A failure does not stop the
# others: at the end EVERYTHING that fails is known. NORTE_SMOKE_JOBS runs
# at a time (4).
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
      rojo "unknown artifact: $artifact"
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
    rojo "FAIL   $artifact $(image_slug "$image") — $log"
    return 1
  fi
}

if [ "${1:-}" = "--one" ]; then
  shift
  una "$@"
  exit
fi

DIR="$(realpath "${1:?usage: smoke.sh DIR [artefacto]}")"
SOLO="${2:-}"
[ -f "$DIR/MANIFEST" ] || { rojo "no $DIR/MANIFEST: is this a build.sh output?"; exit 1; }
REV="$(awk '$1 == "revision" { print $2 }' "$DIR/MANIFEST")"

lineas="$(grep -vE '^[[:space:]]*(#|$)' "$MATRIX" | awk -v solo="$SOLO" 'solo == "" || $1 == solo')"
[ -n "$lineas" ] || { rojo "no matrix line for «$SOLO»"; exit 1; }
[ -n "$SOLO" ] || : >"$DIR/SMOKE"

# `xargs` exits 123 if any run failed, and `set -e` propagates it.
printf '%s\n' "$lineas" |
  xargs -P "${NORTE_SMOKE_JOBS:-4}" -L 1 "$0" --one "$DIR" "$REV"
