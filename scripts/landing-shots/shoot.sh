#!/usr/bin/env bash
# Takes every terminal shot the landing shows, from the build in target/debug.
# `just landing-shots` runs it; see scripts/landing-shots/README.md.
#
# usage: shoot.sh [out-dir]     (default: landing/shots)
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
bin=$repo/target/debug
out=${1:-$repo/landing/shots}
work=$repo/target/landing-shots
home=$work/ada
langs=(en es)
themes=(default catppuccin-mocha catppuccin-latte gruvbox-dark gruvbox-light nord
	retro-crt retro-crt-amber vscode-dark vscode-light)
hero_theme=${HERO_THEME:-catppuccin-mocha}

for tool in bwrap tmux magick zip zstd; do
	command -v "$tool" >/dev/null || { echo "shoot.sh: needs $tool" >&2; exit 1; }
done
[ -x "$bin/ntc" ] && [ -x "$bin/norte" ] || { echo "shoot.sh: build ntc and norte first" >&2; exit 1; }

echo "== ada's home, and the plugins she approved"
rm -rf "$home"
"$here/demo-tree.sh" "$home"
"$here/plugins.sh" "$home" "$bin" "$repo" >/dev/null
"$here/tui.sh" "$home" "$bin" "$here/scenes/approve.scene" "$work/approve" "$hero_theme" en
if [ "$("$here/sandbox.sh" "$home" "$bin" norte plugin list | grep -c 'NOT approved')" != 1 ]; then
	echo "shoot.sh: approve.scene should leave only syntect unapproved" >&2
	"$here/sandbox.sh" "$home" "$bin" norte plugin list >&2
	exit 1
fi

rm -rf "$out/tui"
for lang in "${langs[@]}"; do
	echo "== tour ($lang)"
	"$here/demo-tree.sh" "$home"
	"$here/tui.sh" "$home" "$bin" "$here/scenes/tour.scene" "$out/tui/$lang" "$hero_theme" "$lang"
	"$here/tui.sh" "$home" "$bin" "$here/scenes/grant.scene" "$work/grant" "$hero_theme" "$lang"
	mv "$work/grant/grant.ansi" "$out/tui/$lang/"
	for theme in "${themes[@]}"; do
		echo "== $theme ($lang)"
		"$here/demo-tree.sh" "$home"
		"$here/tui.sh" "$home" "$bin" "$here/scenes/panes.scene" "$work/themes" "$theme" "$lang"
		mkdir -p "$out/tui/$lang/themes"
		mv "$work/themes/panes.ansi" "$out/tui/$lang/themes/$theme.ansi"
	done
done
echo "== done: $out/tui"
