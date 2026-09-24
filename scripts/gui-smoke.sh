#!/usr/bin/env bash
# Smoke test of the package `just gui-package` left on THIS machine, using
# the same body as the baseline's matrix (`scripts/baseline/smoke-inside.sh`).
#
#   scripts/gui-smoke.sh [image]
#
# An image from the Fedora family tests the `.rpm`; any other, the `.deb`.
# This is the development loop: it does not check revision or glibc floor,
# and a package built here does not start on an old distribution. What
# would be published goes through `just baseline` (ADR 0112).
set -euo pipefail
IMAGEN="${1:-debian:trixie}"
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${IMAGEN##*/}" in
  fedora* | rockylinux* | almalinux* | centos*) artefacto=rpm ;;
  *) artefacto=deb ;;
esac
paquete="$(find "$RAIZ/target/release/bundle/$artefacto" -name "*.$artefacto" -print -quit 2>/dev/null || true)"
if [ -z "$paquete" ]; then
  echo "no .$artefacto in target/release/bundle — run \`just gui-package\`" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/gui"
cp "$paquete" "$tmp/gui/"
if ! "$RAIZ/scripts/baseline/smoke.sh" --one "$tmp" "" "$artefacto" "$IMAGEN"; then
  cat "$tmp"/smoke/*.log >&2
  exit 1
fi
