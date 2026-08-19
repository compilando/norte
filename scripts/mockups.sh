#!/usr/bin/env bash
# Renderiza los mockups de docs/mockups a PNG con Chrome headless.
#
#   scripts/mockups.sh              # todos
#   scripts/mockups.sh 01-browse    # sólo los que empiecen por ese prefijo
#
# El lienzo es 1440×900 y se fuerza escala 2 -> PNG de 2880×1800, que es lo que
# hace falta para que el texto mono aguante un zoom en la web.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$root/docs/mockups"
out="$src/png"
prefix="${1:-}"

chrome=""
for c in google-chrome-stable google-chrome chromium chromium-browser; do
  if command -v "$c" >/dev/null 2>&1; then chrome="$c"; break; fi
done
[ -n "$chrome" ] || { echo "ERROR: no encuentro Chrome/Chromium para renderizar."; exit 1; }

mkdir -p "$out"
profile="$(mktemp -d)"
trap 'rm -rf "$profile"' EXIT

shopt -s nullglob
count=0
for f in "$src/${prefix}"*.html; do
  name="$(basename "$f" .html)"
  [ "$name" = "index" ] && continue
  "$chrome" --headless --disable-gpu --hide-scrollbars \
    --user-data-dir="$profile" \
    --force-device-scale-factor=2 \
    --window-size=1440,900 \
    --default-background-color=00000000 \
    --virtual-time-budget=2000 \
    --screenshot="$out/$name.png" \
    "file://$f" >/dev/null 2>&1
  echo "  $name.png"
  count=$((count + 1))
done

echo "$count capturas en docs/mockups/png"
