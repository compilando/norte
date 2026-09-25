#!/usr/bin/env bash
# Plays a scene script against `ntc` in a detached tmux and keeps the screen,
# colours included (`capture-pane -e`), as `.ansi` files the landing renders as
# text. One tmux server of our own (`-L`), so a user's tmux is never touched.
#
# usage: tui.sh <home> <bin-dir> <scene-file> <out-dir> [theme] [lang]
#
# A scene file is one step per line:
#   keys <tmux key>…   send keys (tmux names: Enter, F5, C-g, M-t, Down…)
#   open               Enter, and wait for the directory to load
#   type <text>        type literal text
#   wait <seconds>     let the screen settle
#   shot <name>        write <out-dir>/<name>.ansi
#   frame              append the screen to <out-dir>/reel.ansi (a clip)
#   run <cmd>          start ntc with these arguments (first line)
set -euo pipefail

home=${1:?home} bin=${2:?bin dir} scene=${3:?scene} out=${4:?out dir}
theme=${5:-catppuccin-mocha} lang=${6:-en}
here=$(cd "$(dirname "$0")" && pwd)
sock=norte-landing
cols=${COLS:-132} rows=${ROWS:-38}
settle=${SETTLE:-0.5}
# The shell in the terminal panel speaks the shot's language too.
locale=C.UTF-8
[ "$lang" = es ] && locale=es_ES.UTF-8

mkdir -p "$out"
: >"$out/reel.ansi"
cat >"$home/.config/norte/norte.toml" <<EOF
[ui]
theme = "$theme"
lang = "$lang"
show_hidden = false
row_stripes = true
EOF
# Each run starts with no session, no history and an empty journal.
rm -rf "$home/.local/state" "$home/.cache" "$home"/.config/norte/{journal,index}.db*

t() { tmux -L "$sock" "$@"; }
snap() { t capture-pane -e -p -t shot; }

t kill-server 2>/dev/null || true
while IFS= read -r line || [ -n "$line" ]; do
	[[ -z $line || $line == \#* ]] && continue
	verb=${line%% *}
	arg=${line#"$verb"}
	arg=${arg# }
	case $verb in
	run)
		# No user tmux.conf, and the width of a VS16 emoji decided before ntc
		# draws its first frame.
		t -f /dev/null start-server \; set -s variation-selector-always-wide "${VS16_WIDE:-on}"
		# shellcheck disable=SC2086 # the scene's arguments are words on purpose
		t new-session -d -s shot -x "$cols" -y "$rows" \
			"$here/sandbox.sh" "$home" "$bin" env LANG="$locale" LC_ALL="$locale" ntc $arg
		# Keys sent before the first frame is up are lost, not queued.
		sleep 2.5
		;;
	keys)
		# shellcheck disable=SC2086
		t send-keys -t shot $arg
		sleep "$settle"
		;;
	open)
		# Enter into a directory: the listing loads asynchronously, and a key
		# sent before it lands acts on the OLD one.
		t send-keys -t shot Enter
		sleep 1.2
		;;
	type)
		t send-keys -t shot -l "$arg"
		sleep "$settle"
		;;
	wait) sleep "$arg" ;;
	shot) snap >"$out/$arg.ansi" ;;
	frame)
		snap >>"$out/reel.ansi"
		printf '\f\n' >>"$out/reel.ansi"
		;;
	*)
		echo "tui.sh: unknown step: $line" >&2
		exit 2
		;;
	esac
	# TRACE=1 keeps the screen after every step: the way to see which key missed.
	if [ -n "${TRACE:-}" ]; then
		step=$((${step:-0} + 1))
		{ echo "## $line"; snap; } >"$out/trace-$(printf %03d "$step").ansi"
	fi
done <"$scene"
t kill-server 2>/dev/null || true
[ -s "$out/reel.ansi" ] || rm -f "$out/reel.ansi"
