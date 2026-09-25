#!/usr/bin/env bash
# Runs a command as «ada», in a bwrap sandbox where /home holds only the demo
# home. Setting HOME is not enough: the session state, the config and the
# runtime dir each have their own XDG variable, and one left pointing at the
# real home leaks its folders into the shot — and writes the shot's panes into
# the real session.json.
#
# usage: sandbox.sh <home-dir> <bin-dir> <command> [args…]
set -euo pipefail

home=${1:?home dir}
bin=${2:?bin dir}
shift 2

# Owner columns and `ls -l` read names from passwd: ours says «ada».
ids=$(mktemp -d)
trap 'rm -rf "$ids"' EXIT
printf 'root:x:0:0::/root:/bin/bash\nada:x:%s:%s:Ada:/home/ada:/bin/bash\n' "$(id -u)" "$(id -g)" >"$ids/passwd"
printf 'root:x:0:\nada:x:%s:\n' "$(id -g)" >"$ids/group"

# Only what a program needs is bound, never `/` whole: norte lists drives from
# /proc/mounts, and binding `/` carried every mount of the real machine (the
# user's network drives by name) into the Places sidebar, /home hidden or not.
run=/run/user/$(id -u)
bwrap \
	--die-with-parent \
	--unshare-pid \
	--ro-bind /usr /usr \
	--symlink usr/bin /bin \
	--symlink usr/bin /sbin \
	--symlink usr/lib /lib \
	--symlink usr/lib /lib64 \
	--ro-bind /etc /etc \
	--ro-bind /sys /sys \
	--bind /tmp /tmp \
	--dev /dev \
	--proc /proc \
	--ro-bind "$ids/passwd" /etc/passwd \
	--ro-bind "$ids/group" /etc/group \
	--unshare-uts \
	--hostname norte \
	--tmpfs /home \
	--bind "$home" /home/ada \
	--tmpfs /run \
	--dir "$run" \
	--chmod 0700 "$run" \
	--tmpfs /opt \
	--ro-bind "$bin" /opt/norte \
	--chdir /home/ada \
	--unsetenv XDG_CONFIG_DIRS \
	--setenv HOME /home/ada \
	--setenv USER ada \
	--setenv LOGNAME ada \
	--setenv XDG_CONFIG_HOME /home/ada/.config \
	--setenv XDG_DATA_HOME /home/ada/.local/share \
	--setenv XDG_STATE_HOME /home/ada/.local/state \
	--setenv XDG_CACHE_HOME /home/ada/.cache \
	--setenv XDG_RUNTIME_DIR "$run" \
	--setenv NORTE_NO_WIZARD 1 \
	--setenv NORTE_NO_SPLASH 1 \
	--setenv PATH "/opt/norte:/usr/bin:/bin" \
	--setenv TERM xterm-256color \
	--setenv COLORTERM truecolor \
	--setenv SHELL /bin/bash \
	"$@"
