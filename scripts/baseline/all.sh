#!/usr/bin/env bash
# A ref, end to end: build, smoke test and verification (ADR 0112).
#
#   scripts/baseline/all.sh [ref]
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dir="$("$RAIZ/scripts/baseline/build.sh" "${1:-HEAD}")"
# The smoke test can fail; verify is what says whether the build is good.
"$RAIZ/scripts/baseline/smoke.sh" "$dir" || true
exec "$RAIZ/scripts/baseline/verify.sh" "$dir"
