#!/usr/bin/env bash
# Renders the mockups in docs/mockups to PNG with headless Chrome.
#
#   scripts/mockups.sh              # all of them
#   scripts/mockups.sh 01-browse    # only the ones starting with that prefix
#
# The canvas is 1440×900 and scale 2 is forced -> 2880×1800 PNG, which is
# what it takes for the mono text to hold up to a zoom on the web.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
src="$root/docs/mockups"
out="$src/png"
prefix="${1:-}"

chrome=""
for c in google-chrome-stable google-chrome chromium chromium-browser; do
  if command -v "$c" >/dev/null 2>&1; then chrome="$c"; break; fi
done
[ -n "$chrome" ] || { echo "ERROR: cannot find Chrome/Chromium to render with."; exit 1; }

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

echo "$count captures in docs/mockups/png"
