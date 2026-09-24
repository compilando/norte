#!/usr/bin/env bash
# Can this build be published? Only reads DIR: checksums, glibc floor, each
# binary's revision, and a green smoke test for each matrix line.
#
#   scripts/baseline/verify.sh DIR
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

DIR="$(realpath "${1:?usage: verify.sh DIR}")"
[ -f "$DIR/MANIFEST" ] || { echo "no $DIR/MANIFEST" >&2; exit 1; }

mal=0
if ! manifest_check_sums "$DIR"; then
  echo "SHA256SUMS does not match: something changed after the build" >&2
  mal=1
fi
p="$(manifest_problems "$DIR/MANIFEST")"
if [ -n "$p" ]; then
  printf '%s\n' "$p" >&2
  mal=1
fi
m="$(smoke_missing "$RAIZ/scripts/baseline/matrix.txt" "$DIR/SMOKE")"
if [ -n "$m" ]; then
  printf '%s\n' "$m" | sed 's/^/smoke test not passed: /' >&2
  mal=1
fi
if [ "$mal" -eq 0 ]; then
  printf '\033[32mverified: %s\033[0m\n' "$DIR"
fi
exit "$mal"
