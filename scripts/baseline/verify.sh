#!/usr/bin/env bash
# ¿Se puede publicar esta build? Solo lee DIR: sumas, suelo de glibc,
# revisión de cada binario, y un humo verde por cada línea de la matriz.
#
#   scripts/baseline/verify.sh DIR
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

DIR="$(realpath "${1:?uso: verify.sh DIR}")"
[ -f "$DIR/MANIFEST" ] || { echo "no hay $DIR/MANIFEST" >&2; exit 1; }

mal=0
if ! manifest_check_sums "$DIR"; then
  echo "SHA256SUMS no cuadra: algo cambió después de la build" >&2
  mal=1
fi
p="$(manifest_problems "$DIR/MANIFEST")"
if [ -n "$p" ]; then
  printf '%s\n' "$p" >&2
  mal=1
fi
m="$(smoke_missing "$RAIZ/scripts/baseline/matrix.txt" "$DIR/SMOKE")"
if [ -n "$m" ]; then
  printf '%s\n' "$m" | sed 's/^/humo sin pasar: /' >&2
  mal=1
fi
if [ "$mal" -eq 0 ]; then
  printf '\033[32mverificado: %s\033[0m\n' "$DIR"
fi
exit "$mal"
