#!/usr/bin/env bash
# The release scripts' own check, in seconds and with no network or Docker:
# syntax, shellcheck when it is installed, and the notes of the newest
# version the CHANGELOG has.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"

for s in scripts/release/*.sh; do
    bash -n "$s"
done
if command -v shellcheck > /dev/null; then
    shellcheck scripts/release/*.sh
fi

version="$(awk -F'[][]' '/^## \[[0-9]/ { print $2; exit }' CHANGELOG.md)"
out="$(scripts/release/notes.sh "v$version")"
grep -q "^## \[$version\]" <<< "$out" || { echo "selftest: no CHANGELOG section in the notes" >&2; exit 1; }
grep -q "norte_${version}_amd64.deb" <<< "$out" || { echo "selftest: the version is not in the downloads" >&2; exit 1; }
if grep -q "{VERSION}" <<< "$out"; then
    echo "selftest: a placeholder survived" >&2
    exit 1
fi
if scripts/release/notes.sh v0.0.0-never > /dev/null 2>&1; then
    echo "selftest: a version without a CHANGELOG section was accepted" >&2
    exit 1
fi
# `prep.sh`'s edits, on copies: every version string moves, the CHANGELOG
# gets its section and its compare links, and nothing else changes.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cp Cargo.toml README.md CHANGELOG.md "$tmp/"
current="$(awk -F'"' '/^\[workspace.package\]/ { w = 1 } w && /^version = / { print $2; exit }' Cargo.toml)"
scripts/release/prep.sh --edit-only "$tmp" 9.9.9-selftest.1
if grep -q "\"$current\"" "$tmp/Cargo.toml" || grep -q "v$current" "$tmp/README.md"; then
    echo "selftest: prep left the old version behind" >&2
    exit 1
fi
grep -q '^## \[9.9.9-selftest.1\] - ' "$tmp/CHANGELOG.md" || { echo "selftest: prep wrote no section" >&2; exit 1; }
grep -q "^\[9.9.9-selftest.1\]: .*compare/v$current\.\.\.v9.9.9-selftest.1" "$tmp/CHANGELOG.md" ||
    { echo "selftest: prep wrote no compare link" >&2; exit 1; }
# Header and its blank line, the two new links, the old link gone: five.
[ "$(diff "$tmp/CHANGELOG.md" CHANGELOG.md | grep -c '^[<>]')" -eq 5 ] ||
    { echo "selftest: prep changed more of the CHANGELOG than its header and links" >&2; exit 1; }
echo "release selftest: ok (notes for $version, prep's edits)"
