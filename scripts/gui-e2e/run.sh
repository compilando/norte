#!/usr/bin/env bash
# End-to-end test of the WINDOW: the real `norte-gui` on a virtual X display,
# in a bwrap sandbox with a throwaway home, driven with xdotool, and checked
# against what crossed the bridge (`NORTE_GUI_TRACE`), not against pixels.
# Screenshots are kept for a person to look at.
#
# usage: scripts/gui-e2e/run.sh [scenario.scn…]   (default: every scenario)
# env:   BIN=<dir with norte-gui and norte>  (default: target/debug — the
#        trace exists only in debug builds, so a release binary cannot run it)
#        KEEP=1 keeps the run directory; OUT=<dir> for the screenshots.
# needs: Xvfb, xdotool, bwrap, jq, import (ImageMagick).
#
# A scenario is one step per line (`#` comments):
#   config <toml line>     appended to norte.toml before the window starts
#   file <path>            an empty file under ~/data (folders are made)
#   start [dir]            starts the window on ~/data/<dir> (default: ~/data)
#   key <keys…>            xdotool key names: ctrl+g Escape Return space
#   click <x> <y>          a left click, in window pixels
#   type <text>            types the rest of the line, spaces included
#   wait <seconds>
#   shot <name>            <OUT>/<scenario>-<name>.png
#   expect-query <text>    the box's query, last painted, is exactly <text>
#   expect-marked <n>      the active listing has <n> marked rows
#   expect-shown <text>    the box SHOWS <text>, as laid out (spaces count)
#   expect-preview <text>  the order line's preview contains <text>
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
bin=${BIN:-$root/target/debug}
out=${OUT:-$root/target/gui-e2e}
display=${GUI_DISPLAY:-:78}
mkdir -p "$out"
[ -x "$bin/norte-gui" ] || { echo "no $bin/norte-gui: run 'just link-gui' first" >&2; exit 2; }

scenarios=("$@")
[ ${#scenarios[@]} -gt 0 ] || scenarios=("$root"/scripts/gui-e2e/scenarios/*.scn)

if DISPLAY=$display xdotool getdisplaygeometry > /dev/null 2>&1; then
	echo "gui-e2e: $display is taken; a previous run left its Xvfb behind?" >&2
	exit 2
fi
Xvfb "$display" -screen 0 1400x900x24 -nolisten tcp > /dev/null 2>&1 &
xvfb=$!
app=
work=
stop_app() {
	if [ -n "$app" ]; then
		kill -- "-$app" 2> /dev/null || true
		wait "$app" 2> /dev/null || true
		app=
	fi
}
cleanup() {
	set +e
	stop_app
	kill "$xvfb" 2> /dev/null
	if [ -n "$work" ] && [ -z "${KEEP:-}" ]; then rm -rf "$work"; fi
	return 0
}
trap cleanup EXIT
export DISPLAY=$display
for _ in $(seq 50); do xdotool getdisplaygeometry > /dev/null 2>&1 && break; sleep 0.1; done

# The box's query as last painted: a snapshot carries `goto` at the top, a
# patch carries it as a change; `null` (closed) prints nothing.
last_query() {
	jq -r 'select(.kind == "update") | .value.payload
		| if has("changes") then (.changes[] | select(.change == "goto") | .goto)
		  else .goto end
		| if . == null then "" else .query end' "$1" | tail -1
}

# The marks of the listing that last reported them.
marked_rows() {
	jq -r 'select(.kind == "update") | .value.payload.changes? // empty
		| .[] | select(has("marks")) | .marks' "$1" | tail -1
}

failed=0
for scn in "${scenarios[@]}"; do
	name=$(basename "$scn" .scn)
	work=$(mktemp -d "${TMPDIR:-/tmp}/norte-gui-e2e.XXXXXX")
	home=$work/home trace=$work/trace.jsonl
	mkdir -p "$home/.config/norte" "$home/data" "$work/run"
	chmod 0700 "$work/run"
	printf '[ui]\nsplash = "off"\n' > "$home/.config/norte/norte.toml"
	# Not created here: the window opens it itself, new and owner-only, and
	# refuses one that exists.
	status=ok
	echo "== $name"
	while IFS= read -r line || [ -n "$line" ]; do
		line=${line%%#*}
		read -r verb rest <<< "$line" || true
		[ -n "${verb:-}" ] || continue
		# `type` keeps its spaces: everything after the verb and ONE space.
		text=${line#*"$verb" }
		case $verb in
		config) printf '%s\n' "$rest" >> "$home/.config/norte/norte.toml" ;;
		file) mkdir -p "$(dirname "$home/data/$rest")" && : > "$home/data/$rest" ;;
		start)
			# A process group of its own, so the daemon the window starts
			# goes with it.
			setsid bwrap --die-with-parent --unshare-pid \
				--ro-bind /usr /usr --symlink usr/bin /bin --symlink usr/lib /lib \
				--symlink usr/lib /lib64 --ro-bind /etc /etc --ro-bind /sys /sys \
				--bind /tmp /tmp --dev /dev --proc /proc \
				--tmpfs /home --bind "$home" /home/ada \
				--tmpfs /run --bind "$work/run" /run/user/"$(id -u)" \
				--ro-bind "$bin" /opt/norte --chdir /home/ada \
				--unsetenv XDG_CONFIG_DIRS --setenv HOME /home/ada \
				--setenv XDG_CONFIG_HOME /home/ada/.config \
				--setenv XDG_DATA_HOME /home/ada/.local/share \
				--setenv XDG_STATE_HOME /home/ada/.local/state \
				--setenv XDG_CACHE_HOME /home/ada/.cache \
				--setenv XDG_RUNTIME_DIR /run/user/"$(id -u)" \
				--setenv NORTE_NO_WIZARD 1 --setenv NORTE_NO_SPLASH 1 \
				--setenv NORTE_GUI_TRACE "$trace" \
				--setenv PATH "/opt/norte:/usr/bin:/bin" \
				/opt/norte/norte-gui "/home/ada/data/${rest:-}" \
				> "$work/gui.log" 2>&1 &
			app=$!
			# Up when the window maps and the host has painted once.
			for _ in $(seq 150); do
				if xdotool search --onlyvisible --class norte > /dev/null 2>&1 &&
					jq -e 'select(.kind == "update")' "$trace" > /dev/null 2>&1; then
					break
				fi
				sleep 0.2
			done
			win=$(xdotool search --onlyvisible --class norte | head -1)
			[ -n "$win" ] || { echo "   the window did not come up"; status=fail; break; }
			xdotool windowactivate --sync "$win" 2> /dev/null || xdotool windowfocus "$win"
			sleep 1
			;;
		key) for k in $rest; do xdotool key --clearmodifiers "$k"; sleep 0.15; done ;;
		click)
			# `click <x> <y>`: window-relative pixels, at the window's own size.
			read -r cx cy <<< "$rest"
			xdotool mousemove --window "$win" "$cx" "$cy" click 1
			sleep 0.4
			;;
		type) xdotool type --delay 60 -- "$text"; sleep 0.3 ;;
		wait) sleep "$rest" ;;
		shot) import -window root "$out/$name-$rest.png" ;;
		expect-query)
			sleep 0.5
			got=$(last_query "$trace")
			if [ "$got" != "$text" ]; then
				echo "   FAIL query: expected [$text], got [$got]"
				status=fail
			fi
			;;
		expect-shown)
			# What a person SEES: the bridge can be right while the paint is
			# wrong (spaces collapsed by a flex row, 2026-10-15). The
			# renderer reports the box's `innerText` after each paint, which
			# follows the layout; OCR could not tell a space from none.
			sleep 0.5
			got=$(jq -r 'select(.kind == "dom") | .value.goto' "$trace" | tail -1)
			if [ "$got" != "$text" ]; then
				echo "   FAIL shown: expected [$text], got [$got]"
				status=fail
			fi
			;;
		expect-preview)
			# The order line's preview, last painted, contains <text>.
			sleep 0.5
			got=$(jq -r 'select(.kind == "update") | .value.payload
				| if has("changes") then (.changes[] | select(.change == "goto") | .goto)
				  else .goto end
				| select(. != null) | .order.preview? // empty' "$trace" | tail -1)
			if ! grep -qF -- "$text" <<< "$got"; then
				echo "   FAIL preview: [$text] not in [$got]"
				status=fail
			fi
			;;
		expect-marked)
			sleep 0.5
			got=$(marked_rows "$trace")
			if [ "$got" != "$rest" ]; then
				echo "   FAIL marked: expected $rest, got [${got}]"
				status=fail
			fi
			;;
		*) echo "   unknown step: $verb" >&2; status=fail ;;
		esac
	done < "$scn"
	stop_app
	if [ "$status" = ok ]; then
		echo "   ok"
		rm -rf "$work"
	else
		failed=1
		echo "   trace and log kept in $work"
		import -window root "$out/$name-failure.png" 2> /dev/null || true
	fi
	work=
done
exit "$failed"
