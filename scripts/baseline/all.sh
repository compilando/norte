#!/usr/bin/env bash
# Una referencia, de principio a fin: build, humo y verificación (ADR 0112).
#
#   scripts/baseline/all.sh [ref]
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dir="$("$RAIZ/scripts/baseline/build.sh" "${1:-HEAD}")"
# El humo puede fallar; quien dice si la build vale es verify.
"$RAIZ/scripts/baseline/smoke.sh" "$dir" || true
exec "$RAIZ/scripts/baseline/verify.sh" "$dir"
