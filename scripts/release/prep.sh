#!/usr/bin/env bash
# Prepares a release commit, end to end, so pushing `main` afterwards is
# instant: branch, version bump, CHANGELOG section, the WHOLE `just ci` and
# `just gui-ci` (their seals tell the pre-push hook this exact content already
# passed), and the commit.
#
#   scripts/release/prep.sh 0.3.0-alpha.15
#
# Then: merge the branch into `main` (`git merge --no-ff`), push `main` — the
# hook sees the seals and skips the gate — tag, push the tag, and
# `just release v<version> <lead.md>`.
#
# Why it exists: by hand the bump took a dozen edits, and `just ci` split
# into its recipes (to fit a tool's time limit) never wrote the seal, so the
# push of `main` ran the gate a second time — six to eight minutes for
# nothing, every release.
#
# Run it detached and wait on its PID: `just ci` alone outlasts a ten-minute
# foreground call.
set -euo pipefail

usage="usage: prep.sh <version without v, e.g. 0.3.0-alpha.15> | --edit-only <dir> <version>"
edit_only=
if [ "${1:-}" = --edit-only ]; then
	# The selftest's door: only the edits, on copies in <dir>.
	edit_only="${2:?$usage}"
	shift 2
fi
new="${1:?$usage}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "${edit_only:-$root}"

if [ -z "$edit_only" ]; then
	[ -z "$(git status --porcelain)" ] || { echo "prep: the tree is not clean" >&2; exit 1; }
	[ "$(git branch --show-current)" = main ] || { echo "prep: start from main" >&2; exit 1; }
fi
old="$(awk -F'"' '/^\[workspace.package\]/ { w = 1 } w && /^version = / { print $2; exit }' Cargo.toml)"
[ -n "$old" ] || { echo "prep: no workspace version in Cargo.toml" >&2; exit 1; }
[ "$old" != "$new" ] || { echo "prep: already at $new" >&2; exit 1; }
grep -q '^## \[Unreleased\]' CHANGELOG.md || { echo "prep: CHANGELOG has no [Unreleased]" >&2; exit 1; }
if grep -q "^## \[$new\]" CHANGELOG.md; then
	echo "prep: CHANGELOG already has $new" >&2
	exit 1
fi

[ -n "$edit_only" ] || git switch -c "chore/release-$new"

# The workspace version and every `version = "<old>"` of our path crates.
sed -i "s/\"$old\"/\"$new\"/g" Cargo.toml
sed -i "s/v$old/v$new/g" README.md
today="$(date +%F)"
sed -i "0,/^## \[Unreleased\]/s//## [Unreleased]\n\n## [$new] - $today/" CHANGELOG.md
sed -i "s#^\[Unreleased\]: \(.*\)/compare/v$old\.\.\.HEAD#[Unreleased]: \1/compare/v$new...HEAD\n[$new]: \1/compare/v$old...v$new#" CHANGELOG.md
grep -q "^\[$new\]: " CHANGELOG.md || { echo "prep: the CHANGELOG links were not updated" >&2; exit 1; }
[ -z "$edit_only" ] || exit 0

scripts/release/selftest.sh
# The whole gate, as ONE run each: their seals are what makes the push of
# `main` instant. `ci` leaves the window out on purpose; `gui-ci` covers it.
just ci
just gui-ci

git add Cargo.toml Cargo.lock README.md CHANGELOG.md
msg="$(mktemp)"
trap 'rm -f "$msg"' EXIT
printf 'chore(release): v%s\n\nCo-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>\n' "$new" > "$msg"
git commit -q -F "$msg"
echo "prep: v$new committed on chore/release-$new; ci and gui-ci sealed."
echo "next: git switch main && git merge --no-ff chore/release-$new && git push origin main"
echo "      git tag -a v$new -m v$new && git push origin v$new && just release v$new <lead.md>"
