#!/usr/bin/env bash
# Takes every terminal shot the landing shows, from the build in target/debug.
# `just landing-shots` runs it; see scripts/landing-shots/README.md.
#
# usage: shoot.sh [out-dir]     (default: landing/shots)
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
build=$repo/target/debug
out=${1:-$repo/landing/shots}
# On tmpfs, with the binaries copied in: norte lists drives from /proc/mounts
# and hides the pseudo ones, so ada's home and /opt/norte do not show up in
# the Places sidebar as two disks of their own.
work=${LANDING_SHOTS_WORK:-/tmp/norte-landing-shots}
home=$work/ada
bin=$work/bin
langs=(en es)
themes=(default catppuccin-mocha catppuccin-latte gruvbox-dark gruvbox-light nord
	retro-crt retro-crt-amber vscode-dark vscode-light)
hero_theme=${HERO_THEME:-catppuccin-mocha}

for tool in bwrap tmux magick zip zstd; do
	command -v "$tool" >/dev/null || { echo "shoot.sh: needs $tool" >&2; exit 1; }
done
[ -x "$build/ntc" ] && [ -x "$build/norte" ] || { echo "shoot.sh: build ntc and norte first" >&2; exit 1; }
mkdir -p "$bin"
for b in ntc norte norte-gui; do
	if [ -x "$build/$b" ]; then cp "$build/$b" "$bin/"; fi
done

echo "== ada's home, and the plugins she approved"
rm -rf "$home"
"$here/demo-tree.sh" "$home"
"$here/plugins.sh" "$home" "$bin" "$repo" >/dev/null
"$here/tui.sh" "$home" "$bin" "$here/scenes/approve.scene" "$work/approve" "$hero_theme" en
if [ "$("$here/sandbox.sh" "$home" "$bin" norte plugin list | grep -c 'NOT approved')" != 1 ]; then
	echo "shoot.sh: approve.scene should leave only media-info unapproved" >&2
	"$here/sandbox.sh" "$home" "$bin" norte plugin list >&2
	exit 1
fi

# ONLY=tui or ONLY=gui retakes one half and leaves the other as it was.
[ "${ONLY:-}" = gui ] || rm -rf "$out/tui"
for lang in "${langs[@]}"; do
	[ "${ONLY:-}" = gui ] && break
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
if [ "${ONLY:-}" = tui ]; then
	exit 0
fi
if ! command -v Xvfb >/dev/null || [ ! -x "$bin/norte-gui" ]; then
	echo "== no Xvfb or no norte-gui: the window's shots are left as they were"
	exit 0
fi

# The window: a still per theme, and per language a recorded tour with its stills.
gui_out=$repo/landing/public/shots/gui
webp() { magick "$1" -quality 84 "$2"; }
rm -rf "$gui_out"
for lang in "${langs[@]}"; do
	mkdir -p "$gui_out/$lang"
	echo "== window tour ($lang)"
	"$here/demo-tree.sh" "$home"
	rm -rf "$work/gui"
	RECORD=$gui_out/$lang/tour.webm "$here/gui.sh" "$home" "$bin" "$here/scenes/gui-tour.scene" "$work/gui" "$hero_theme" "$lang"
	for png in "$work"/gui/*.png; do webp "$png" "$gui_out/$lang/$(basename "$png" .png).webp"; done
	for theme in "${themes[@]}"; do
		echo "== window $theme ($lang)"
		"$here/demo-tree.sh" "$home"
		rm -rf "$work/gui"
		"$here/gui.sh" "$home" "$bin" "$here/scenes/gui-panes.scene" "$work/gui" "$theme" "$lang"
		webp "$work/gui/panes.png" "$gui_out/$lang/panes-$theme.webp"
	done
done
echo "== done: $out/tui, $gui_out"
