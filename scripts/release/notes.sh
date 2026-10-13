#!/usr/bin/env bash
# Release notes for <tag>: a lead paragraph, the downloads block with the
# version filled in, and the tag's CHANGELOG section. Prints to stdout.
#
#   scripts/release/notes.sh v0.3.0-beta.1 [lead.md]
#
# The lead is the one part a person writes; without it the notes say
# "Pre-release." (or nothing for a stable tag) and the draft is edited
# before publishing. Fails if the CHANGELOG has no section for the version:
# a release without its changes written down is not ready.
set -euo pipefail

tag="${1:?usage: notes.sh <tag> [lead.md]}"
lead="${2:-}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
version="${tag#v}"

section="$(awk -v v="$version" '
    $0 ~ "^## \\[" v "\\]" { on = 1; print; next }
    on && /^## \[/ { exit }
    on { print }
' "$root/CHANGELOG.md")"
if [ -z "$section" ]; then
    echo "notes: CHANGELOG.md has no section '## [$version]'" >&2
    exit 1
fi

if [ -n "$lead" ]; then
    cat "$lead"
elif [[ "$version" == *-* ]]; then
    echo "Pre-release."
fi
echo
sed "s/{VERSION}/$version/g" "$root/scripts/release/downloads.md"
echo "---"
echo
printf '%s\n' "$section"
