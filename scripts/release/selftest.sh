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
echo "release selftest: ok (notes for $version)"
