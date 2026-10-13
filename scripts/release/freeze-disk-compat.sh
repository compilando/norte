#!/usr/bin/env bash
# Freezes tests/disk-compat/<version>/ with a RELEASED `norte` binary
# (ADR 0182, tests/disk-compat/README.md).
#
#   scripts/release/freeze-disk-compat.sh 0.3.0-beta.1 path/to/released/norte [dest]
#
# - The text configs are copied from the newest corpus already there: add by
#   hand whatever the release made new (a key, a file kind).
# - The journal and the index are WRITTEN by the given binary, inside bwrap,
#   so they record `/srv/norte/...` and not this machine's home.
# - `expected/commands.txt` comes from the TAG's catalogue source, not this
#   tree's: a rename after the tag must not be frozen under its new name.
# - A third argument writes somewhere else, to try the script without
#   touching the real corpora.
#
# Needs bwrap, sqlite3, cpio and the tag `v<version>` in this repository.
set -euo pipefail

version="${1:?usage: freeze-disk-compat.sh <version> <norte-binary> [dest]}"
binary="$(realpath "${2:?usage: freeze-disk-compat.sh <version> <norte-binary> [dest]}")"
root="$(cd "$(dirname "$0")/../.." && pwd)"
corpora="$root/tests/disk-compat"
dest="${3:-$corpora/$version}"
tag="v$version"

[ -e "$dest" ] && { echo "freeze: $dest already exists; a corpus is never rewritten" >&2; exit 1; }
for tool in bwrap sqlite3 cpio; do
    command -v "$tool" > /dev/null || { echo "freeze: needs $tool" >&2; exit 1; }
done
"$binary" --version | grep -qF "norte $version " || {
    echo "freeze: $binary is not norte $version: $("$binary" --version)" >&2
    exit 1
}
git -C "$root" rev-parse -q --verify "refs/tags/$tag" > /dev/null || {
    echo "freeze: no tag $tag to read the catalogue from" >&2
    exit 1
}
# Semver order, not `sort -V`'s: a prerelease (`-rc.1`) sorts BEFORE its
# release. `~` sorts before everything for `sort -V`, so it stands in for
# the first `-` while sorting.
prev="$(find "$corpora" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' \
    | sed 's/-/~/' | sort -V | sed 's/~/-/' | tail -1)"
[ -n "$prev" ] || { echo "freeze: no previous corpus to start from" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/c" "$work/s" "$work/h" "$work/w"
printf 'hola, norte\n' > "$work/w/a.txt"
printf 'adiós, ñandú\n' > "$work/w/b ñ.txt"
cp "$binary" "$work/norte"
cat > "$work/gen.sh" << 'GEN'
set -eu
export NORTE_CONFIG_DIR=/srv/norte/c XDG_STATE_HOME=/srv/norte/s NORTE_LANG=en
B=/srv/norte/norte
W=/srv/norte/w
$B mkdir $W/dir
$B cp $W/a.txt $W/dir/a.txt
$B mv "$W/b ñ.txt" $W/dir/b.txt
$B rm $W/a.txt
$B mkdir "$W/lat$(printf '\351')n"
$B cp $W/dir $W/copia
$B index build $W > /dev/null 2>&1
# Folded into the main file, so copying the `.db` alone is complete. The
# index stays in WAL mode, like a user's.
sqlite3 /srv/norte/c/index.db "PRAGMA wal_checkpoint(TRUNCATE);" > /dev/null
sqlite3 /srv/norte/c/journal.db "PRAGMA wal_checkpoint(TRUNCATE);" > /dev/null
$B audit export --format jsonl > /srv/norte/journal.jsonl
$B index query /srv/norte/w copia > /srv/norte/index-query-copia.txt
GEN
# HOME and the XDG dirs point inside too: the binary sees nothing of this
# machine but the bound paths.
bwrap --dev-bind / / --tmpfs /srv --bind "$work" /srv/norte \
    --setenv HOME /srv/norte/h --setenv XDG_DATA_HOME /srv/norte/h \
    --setenv XDG_CACHE_HOME /srv/norte/h --setenv XDG_RUNTIME_DIR /srv/norte/h \
    bash /srv/norte/gen.sh

# Built aside and moved at the end: a failure leaves no half corpus that
# a rerun would then call "already frozen".
out="$work/out"
mkdir -p "$out/config" "$out/expected"
(cd "$corpora/$prev/config" && find . -type f ! -name '*.db' -print0 | cpio -0pdm --quiet "$out/config")
cp "$work/c/journal.db" "$work/c/index.db" "$out/config/"
cp "$work/journal.jsonl" "$work/index-query-copia.txt" "$out/expected/"
cp "$corpora/$prev/expected/doctor-warnings.txt" "$out/expected/"
git -C "$root" show "$tag:crates/norte-frontend/src/keymap/catalogue.rs" \
    | awk '/^pub const CATALOGUE/,/^];/' \
    | grep -Eo '\b[a-z_]+\(\s*"[a-z0-9.:-]+"' \
    | sed -E 's/.*"([^"]+)"/\1/' > "$out/expected/commands.txt"
[ -s "$out/expected/commands.txt" ] || { echo "freeze: no command names read from $tag" >&2; exit 1; }
mkdir -p "$(dirname "$dest")"
mv "$out" "$dest"

echo "frozen: $dest (text configs from $prev, $(wc -l < "$dest/expected/commands.txt") commands)"
echo "next: add what $version made new to $dest/config, then"
echo "      just t norte-cli && just t norte-frontend   # every corpus still loads"
