#!/usr/bin/env bash
# Humo del paquete que `just gui-package` dejó en ESTA máquina, por el mismo
# cuerpo que la matriz de la base (`scripts/baseline/smoke-inside.sh`).
#
#   scripts/gui-smoke.sh [imagen]
#
# Una imagen de la familia Fedora prueba el `.rpm`; cualquier otra, el `.deb`.
# Es el bucle de desarrollo: no comprueba revisión ni suelo de glibc, y un
# paquete construido aquí no arranca en una distribución vieja. Lo que se
# publicaría pasa por `just baseline` (ADR 0112).
set -euo pipefail
IMAGEN="${1:-debian:trixie}"
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "${IMAGEN##*/}" in
  fedora* | rockylinux* | almalinux* | centos*) artefacto=rpm ;;
  *) artefacto=deb ;;
esac
paquete="$(find "$RAIZ/target/release/bundle/$artefacto" -name "*.$artefacto" -print -quit 2>/dev/null || true)"
if [ -z "$paquete" ]; then
  echo "no hay .$artefacto en target/release/bundle — corre \`just gui-package\`" >&2
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
