#!/usr/bin/env bash
# End-to-end test of the WINDOW: the real `norte-gui` on a virtual X display,
# in a bwrap sandbox with a throwaway home, driven with xdotool, and checked
# against what crossed the bridge (`NORTE_GUI_TRACE`) and what the renderer
# reports it SHOWS (the `dom` probes, `ui/src/probes.ts`), not against pixels.
# Screenshots are kept for a person to look at. This is X11, and it paints
# (see WEBKIT_DISABLE_DMABUF_RENDERER below); #261 is the matrix it covers,
# `docs/gui-matrix-261.md` what it does not.
#
# usage: scripts/gui-e2e/run.sh [scenario.scn…]   (default: every scenario)
# env:   BIN=<dir with norte-gui and norte>  (default: target/debug — the
#        trace exists only in debug builds, so a release binary cannot run it)
#        KEEP=1 keeps the run directory; OUT=<dir> for the screenshots;
#        GUI_DISPLAY=:N for the Xvfb display (default :78).
# needs: Xvfb, xdotool, bwrap, jq, import (ImageMagick); xclip for `clip-*`.
#
# A scenario is one step per line (`#` comments):
#   config <toml line>     appended to norte.toml before the window starts
#   env <NAME>=<value>     set in the window's environment (GDK_SCALE=2)
#   file <path>            an empty file under ~/data (folders are made)
#   start [dir]            starts the window on ~/data/<dir> (default: ~/data)
#   restart                stops the window (and its daemon) and starts it
#                          again WITHOUT a folder, so the session decides
#   norte <args…>          runs the CLI in the same sandbox (`daemon stop
#                          --handover`); fails the scenario if it fails
#   key <keys…>            xdotool key names: ctrl+g Escape Return space
#   click <x> <y> [mod]    a left click, in window pixels, holding <mod>
#   rclick <x> <y>         a right click
#   drag <x1> <y1> <x2> <y2>  left button down, move, up
#   type <text>            types the rest of the line, spaces included
#   clip-set <text>        puts <text> on the CLIPBOARD (xclip)
#   wait <seconds>
#   shot <name>            <OUT>/<scenario>-<name>.png
# Oracles poll for up to 10 s, then fail with what they last saw:
#   expect-query <text>    the box's query, last painted, is exactly <text>
#   expect-marked <n>      the active listing has <n> marked rows
#   expect-shown <text>    the box SHOWS <text>, as laid out (spaces count)
#   expect-preview <text>  the order line's preview contains <text>
#   expect-dom <probe> <text>       a probe is exactly <text>
#   expect-dom-has <probe> <text>   a probe contains <text>
#   expect-dom-lacks <probe> <text> a probe does not contain <text>
#                          probes: goto active cursor slots menu menubar lang context
#                          dialog open focus tone dpr (`ui/src/probes.ts`)
#   expect-clip <text>     the CLIPBOARD contains <text>
#   expect-jq <filter>     `jq -s -e <filter>` over the trace holds
#   expect-file <path>     ~/data/<path> exists (expect-no-file: does not)
#   expect-tab <text>      some tab, in the layout last painted, is titled <text>
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
# Room for the window at GDK_SCALE=2 (1200×800 logical): with no window
# manager nothing keeps it on screen, and a dialog centred in it fell off.
Xvfb "$display" -screen 0 2560x1700x24 -nolisten tcp > /dev/null 2>&1 &
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
	# Gone before we return: a run started right after found the display
	# free while its lock was still held, and its own Xvfb died (GTK then
	# failed to initialise).
	wait "$xvfb" 2> /dev/null
	if [ -n "$work" ] && [ -z "${KEEP:-}" ]; then rm -rf "$work"; fi
	return 0
}
trap cleanup EXIT
export DISPLAY=$display
for _ in $(seq 50); do xdotool getdisplaygeometry > /dev/null 2>&1 && break; sleep 0.1; done
kill -0 "$xvfb" 2> /dev/null || { echo "gui-e2e: Xvfb on $display did not start" >&2; exit 2; }

# The sandbox: a throwaway home at /home/ada, its own runtime dir, the
# binaries read-only at /opt/norte. No Wayland socket is reachable (/run is
# a tmpfs) and none is advertised, so GTK and the clipboard helpers take X.
# WEBKIT_DISABLE_DMABUF_RENDERER: without it WebKitGTK's DMA-BUF renderer
# lays the page out and paints NOTHING on an X server without DRI3 (Xvfb)
# — the probes read a full DOM and every screenshot is blank (#261).
sandbox() {
	bwrap --die-with-parent --unshare-pid \
		--ro-bind /usr /usr --symlink usr/bin /bin --symlink usr/lib /lib \
		--symlink usr/lib /lib64 --ro-bind /etc /etc --ro-bind /sys /sys \
		--bind /tmp /tmp --dev /dev --proc /proc \
		--tmpfs /home --bind "$home" /home/ada \
		--tmpfs /run --bind "$work/run" /run/user/"$(id -u)" \
		--ro-bind "$bin" /opt/norte --chdir /home/ada \
		--unsetenv XDG_CONFIG_DIRS --unsetenv WAYLAND_DISPLAY \
		--setenv WEBKIT_DISABLE_DMABUF_RENDERER 1 --setenv HOME /home/ada \
		--setenv XDG_CONFIG_HOME /home/ada/.config \
		--setenv XDG_DATA_HOME /home/ada/.local/share \
		--setenv XDG_STATE_HOME /home/ada/.local/state \
		--setenv XDG_CACHE_HOME /home/ada/.cache \
		--setenv XDG_RUNTIME_DIR /run/user/"$(id -u)" \
		--setenv NORTE_NO_WIZARD 1 --setenv NORTE_NO_SPLASH 1 \
		--setenv PATH "/opt/norte:/usr/bin:/bin" \
		"$@"
}

# Starts the window, with these arguments, and waits until it maps and the
# host has painted once.
launch() {
	# A process group of its own, so the daemon the window starts goes
	# with it.
	export -f sandbox
	export home work bin
	setsid bash -c 'sandbox "$@"' _ "${extra_env[@]}" --setenv NORTE_GUI_TRACE "$trace" \
		/opt/norte/norte-gui "$@" >> "$work/gui.log" 2>&1 &
	app=$!
	for _ in $(seq 150); do
		if xdotool search --onlyvisible --class norte > /dev/null 2>&1 &&
			jq -e 'select(.kind == "update")' "$trace" > /dev/null 2>&1; then
			break
		fi
		sleep 0.2
	done
	win=$(xdotool search --onlyvisible --class norte | head -1)
	[ -n "$win" ] || return 1
	xdotool windowactivate --sync "$win" 2> /dev/null || xdotool windowfocus "$win"
	sleep 1
}

# The box's query as last painted: a snapshot carries `goto` at the top, a
# patch carries it as a change; `null` (closed) prints nothing.
last_query() {
	jq -r 'select(.kind == "update") | .value.payload
		| if has("changes") then (.changes[] | select(.change == "goto") | .goto)
		  else .goto end
		| if . == null then "" else .query end' "$trace" | tail -1
}

# The marks of the listing that last reported them.
marked_rows() {
	jq -r 'select(.kind == "update") | .value.payload.changes? // empty
		| .[] | select(has("marks")) | .marks' "$trace" | tail -1
}

last_preview() {
	jq -r 'select(.kind == "update") | .value.payload
		| if has("changes") then (.changes[] | select(.change == "goto") | .goto)
		  else .goto end
		| select(. != null) | .order.preview? // empty' "$trace" | tail -1
}

# The tab titles of the last layout painted (a snapshot's, or a `layout`
# change's), one per line.
last_tabs() {
	jq -c 'select(.kind == "update") | .value.payload
		| if has("changes") then (.changes[] | select(.change == "layout") | .tabs)
		  else .layout.tabs end
		| select(. != null) | [.[].tabs[].title]' "$trace" | tail -1 | jq -r '.[]?'
}

# A probe's value in the last `dom` report.
probe() {
	jq -r --arg p "$1" 'select(.kind == "dom") | .value[$p] // empty' "$trace" | tail -1
}

clipboard() { xclip -o -selection clipboard 2> /dev/null || true; }

jq_holds() { jq -s -e "$1" "$trace" > /dev/null 2>&1 && echo yes || echo no; }

file_exists() { [ -e "$home/data/$1" ] && echo yes || echo no; }

# expect <label> <exact|has|lacks> <want> <getter…>: polls the getter for up
# to 10 s. A wall-clock oracle that checked once lost its race under load,
# and at GDK_SCALE=2 a software-rendered frame took longer than 3 s.
expect() {
	local label=$1 mode=$2 want=$3 got=
	shift 3
	for _ in $(seq 50); do
		got=$("$@")
		case $mode in
		exact) [ "$got" = "$want" ] && return 0 ;;
		has) [[ $got == *"$want"* ]] && return 0 ;;
		lacks) [[ $got != *"$want"* ]] && return 0 ;;
		esac
		sleep 0.2
	done
	echo "   FAIL $label: expected $mode [$want], got [$got]"
	status=fail
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
	runs=0
	extra_env=()
	echo "== $name"
	while IFS= read -r line || [ -n "$line" ]; do
		line=${line%%#*}
		read -r verb rest <<< "$line" || true
		[ -n "${verb:-}" ] || continue
		# `type` keeps its spaces: everything after the verb and ONE space.
		text=${line#*"$verb" }
		# The oracles that name a probe: `<probe> <text…>`.
		read -r pname ptext <<< "$rest" || true
		ptext=${text#*"$pname" }
		[ "$ptext" != "$text" ] || ptext=
		case $verb in
		config) printf '%s\n' "$rest" >> "$home/.config/norte/norte.toml" ;;
		env) extra_env+=(--setenv "${rest%%=*}" "${rest#*=}") ;;
		file) mkdir -p "$(dirname "$home/data/$rest")" && : > "$home/data/$rest" ;;
		start)
			launch "/home/ada/data/${rest:-}" || { echo "   the window did not come up"; status=fail; break; }
			;;
		restart)
			# The trace is opened NEW by each window; the old one is kept.
			stop_app
			runs=$((runs + 1))
			mv "$trace" "$work/trace.$runs.jsonl"
			launch || { echo "   the window did not come back"; status=fail; break; }
			;;
		norte)
			export -f sandbox
			export home work bin
			# shellcheck disable=SC2086 # word splitting is the point
			if ! timeout 30 bash -c 'sandbox /opt/norte/norte "$@"' _ $rest >> "$work/cli.log" 2>&1; then
				echo "   FAIL norte $rest (see cli.log)"
				status=fail
			fi
			;;
		key) for k in $rest; do xdotool key --clearmodifiers "$k"; sleep 0.15; done ;;
		click | rclick)
			# Window-relative pixels, at the window's own size; a third word
			# is a modifier held through the click (`shift`, `ctrl`).
			read -r cx cy mod <<< "$rest"
			button=1
			[ "$verb" = click ] || button=3
			xdotool mousemove --window "$win" "$cx" "$cy"
			[ -z "${mod:-}" ] || xdotool keydown "$mod"
			xdotool click "$button"
			[ -z "${mod:-}" ] || xdotool keyup "$mod"
			sleep 0.4
			;;
		drag)
			read -r x1 y1 x2 y2 <<< "$rest"
			xdotool mousemove --window "$win" "$x1" "$y1" mousedown 1
			sleep 0.2
			# In steps: one jump is not a drag to a renderer that tracks moves.
			for i in 1 2 3 4 5; do
				xdotool mousemove --window "$win" $((x1 + (x2 - x1) * i / 5)) $((y1 + (y2 - y1) * i / 5))
				sleep 0.1
			done
			xdotool mouseup 1
			sleep 0.4
			;;
		type) xdotool type --delay 60 -- "$text"; sleep 0.3 ;;
		clip-set) printf '%s' "$text" | xclip -selection clipboard -i ;;
		wait) sleep "$rest" ;;
		shot) import -window "$win" "$out/$name-$rest.png" ;;
		expect-query) expect query exact "$text" last_query ;;
		# What a person SEES: the bridge can be right while the paint is
		# wrong (spaces collapsed by a flex row, 2026-10-15).
		expect-shown) expect shown exact "$text" probe goto ;;
		expect-preview) expect preview has "$text" last_preview ;;
		expect-marked) expect marked exact "$rest" marked_rows ;;
		expect-dom) expect "dom $pname" exact "$ptext" probe "$pname" ;;
		expect-dom-has) expect "dom $pname" has "$ptext" probe "$pname" ;;
		expect-dom-lacks) expect "dom $pname" lacks "$ptext" probe "$pname" ;;
		expect-clip) expect clipboard has "$text" clipboard ;;
		expect-jq) expect "jq $text" exact yes jq_holds "$text" ;;
		expect-file) expect "file $rest" exact yes file_exists "$rest" ;;
		expect-no-file) expect "no file $rest" exact no file_exists "$rest" ;;
		expect-tab) expect tab has "$text" last_tabs ;;
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
