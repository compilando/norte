#!/usr/bin/env bash
# Plays a scene script against `norte-gui` on a virtual X display and keeps
# the window as PNG. Same scene format as tui.sh; the tmux key names are
# translated to xdotool's. `frame` is ignored: the window's clip is a video
# (RECORD=<file.webm> records the whole scene).
#
# usage: gui.sh <home> <bin-dir> <scene-file> <out-dir> [theme] [lang]
# needs: Xvfb, xdotool, import (ImageMagick), and ffmpeg for RECORD.
set -euo pipefail

home=${1:?home} bin=${2:?bin dir} scene=${3:?scene} out=${4:?out dir}
theme=${5:-catppuccin-mocha} lang=${6:-en}
here=$(cd "$(dirname "$0")" && pwd)
width=${WIDTH:-1440} height=${HEIGHT:-900}
settle=${SETTLE:-0.6}
display=${GUI_DISPLAY:-:77}

mkdir -p "$out"
cat >"$home/.config/norte/norte.toml" <<EOF
[ui]
theme = "$theme"
lang = "$lang"
show_hidden = false
row_stripes = true
splash = "off"
EOF
rm -rf "$home/.local/state" "$home/.cache" "$home"/.config/norte/{journal,index}.db*

Xvfb "$display" -screen 0 "${width}x${height}x24" -nolisten tcp >/dev/null 2>&1 &
xvfb=$!
app=
rec=
cleanup() {
	[ -n "$rec" ] && kill -INT "$rec" 2>/dev/null && wait "$rec" 2>/dev/null
	[ -n "$app" ] && kill "$app" 2>/dev/null
	kill "$xvfb" 2>/dev/null || true
}
trap cleanup EXIT
export DISPLAY=$display
for _ in $(seq 50); do xdotool getdisplaygeometry >/dev/null 2>&1 && break; sleep 0.1; done

# tmux key names → xdotool's.
key() {
	local k=$1
	case $k in
	Enter) k=Return ;;
	Escape) k=Escape ;;
	C-M-*) k=ctrl+alt+${k#C-M-} ;;
	C-*) k=ctrl+${k#C-} ;;
	M-*) k=alt+${k#M-} ;;
	PageDown | NPage) k=Next ;;
	PageUp | PPage) k=Prior ;;
	esac
	xdotool key --clearmodifiers "$k"
}

while IFS= read -r line || [ -n "$line" ]; do
	[[ -z $line || $line == \#* ]] && continue
	verb=${line%% *}
	arg=${line#"$verb"}
	arg=${arg# }
	case $verb in
	run)
		# shellcheck disable=SC2086 # the scene's arguments are words on purpose
		"$here/sandbox.sh" "$home" "$bin" env DISPLAY="$display" GDK_BACKEND=x11 \
			WEBKIT_DISABLE_DMABUF_RENDERER=1 LIBGL_ALWAYS_SOFTWARE=1 \
			norte-gui --no-splash $arg >"$out/gui.log" 2>&1 &
		app=$!
		win=$(xdotool search --sync --onlyvisible --name norte | head -1)
		xdotool windowmove "$win" 0 0 windowsize "$win" "$width" "$height" windowfocus "$win"
		# The daemon starts, the webview loads, the first listing arrives.
		sleep 6
		if [ -n "${RECORD:-}" ]; then
			ffmpeg -loglevel error -y -f x11grab -framerate 24 -video_size "${width}x${height}" -i "$display" \
				-c:v libvpx-vp9 -b:v 0 -crf 38 -row-mt 1 -deadline realtime -pix_fmt yuv420p "$RECORD" &
			rec=$!
		fi
		;;
	keys)
		for k in $arg; do key "$k"; done
		sleep "$settle"
		;;
	open)
		key Enter
		sleep 1.2
		;;
	type)
		xdotool type --delay 40 "$arg"
		sleep "$settle"
		;;
	wait) sleep "$arg" ;;
	shot) import -window root "$out/$arg.png" ;;
	frame) ;;
	*)
		echo "gui.sh: unknown step: $line" >&2
		exit 2
		;;
	esac
done <"$scene"
