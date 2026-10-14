#!/usr/bin/env bash
# Cuts the release of an already-pushed tag, end to end, and leaves it as
# a DRAFT: publishing stays a person's call.
#
#   scripts/release/release.sh v0.3.0-beta.1 [lead.md]
#
# The steps used to be done by hand (notes, `just baseline`, `just
# baseline-publish`, the Windows workflow, its checksums renamed, the
# disk-compat corpus). Each step checks whether it is already done, so a
# run that dies halfway is resumed by running it again; the Windows run of
# the tag is found again by its title rather than dispatched twice.
#
# Run it in the FOREGROUND: `just baseline` alone outlasts the five-minute
# background cap. Needs gh (authenticated), docker, bwrap, sqlite3, cpio.
set -euo pipefail

tag="${1:?usage: release.sh <tag> [lead.md]}"
lead="${2:-}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
version="${tag#v}"
cd "$root"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

say() { printf '\n== %s\n' "$*"; }
assets() { gh release view "$tag" --json assets -q '.assets[].name' 2> /dev/null || true; }
has_asset() {
    local names
    names="$(assets)"
    grep -qxF "$1" <<< "$names"
}
# Every upload goes through here: a published release is never touched.
must_be_draft() {
    [ "$(gh release view "$tag" --json isDraft -q .isDraft)" = true ] || {
        echo "$tag is already PUBLISHED: refusing to upload into it" >&2
        exit 1
    }
}

say "preflight"
git rev-parse -q --verify "refs/tags/$tag" > /dev/null || { echo "no local tag $tag" >&2; exit 1; }
git ls-remote --exit-code --tags origin "refs/tags/$tag" > /dev/null || { echo "$tag is not pushed" >&2; exit 1; }
tagged="$(git show "$tag:Cargo.toml" | awk -F'"' '/^version = / { print $2; exit }')"
[ "$tagged" = "$version" ] || { echo "$tag's Cargo.toml says $tagged" >&2; exit 1; }
gh auth status > /dev/null
# cargo-dist's `release.yml` creates a PUBLISHED release with these same
# file names on a tag push. It is disabled on GitHub; if anyone turns it
# back on, this script and it would race over one release.
if [ "$(gh workflow view release.yml --json state -q .state 2> /dev/null)" = active ]; then
    echo "release.yml (cargo-dist) is active: disable it, it publishes on tag push" >&2
    exit 1
fi
scripts/release/notes.sh "$tag" "$lead" > "$tmp/notes.md"

say "draft release"
if gh release view "$tag" > /dev/null 2>&1; then
    echo "already there"
else
    pre=()
    if [[ "$version" == *-* ]]; then pre=(--prerelease); fi
    gh release create "$tag" --draft --verify-tag ${pre[@]+"${pre[@]}"} --title "$tag" \
        --notes-file "$tmp/notes.md"
fi
must_be_draft

say "Windows packages (GitHub runner, ~35 min): started now, collected at the end"
run_id=""
if ! has_asset SHA256SUMS-windows; then
    # This tag's run, if one is queued, running or already green — and
    # started after the tag was made: a tag moved before publishing has an
    # older run with the same title, built from the old commit. The run's
    # own sha is main's at dispatch, not the tag's, so time is the check.
    tag_date="$(git for-each-ref --format='%(creatordate:iso-strict)' "refs/tags/$tag")"
    tag_epoch="$(date -d "$tag_date" +%s)"
    find_run() {
        gh run list --workflow windows.yml --event workflow_dispatch -L 20 \
            --json databaseId,displayTitle,status,conclusion,createdAt \
            -q "[.[] | select(.displayTitle == \"Windows packages $tag\")
                     | select(.createdAt | fromdateiso8601 >= $tag_epoch)
                     | select(.conclusion != \"failure\" and .conclusion != \"cancelled\")][0].databaseId // empty"
    }
    run_id="$(find_run)"
    if [ -z "$run_id" ]; then
        gh workflow run windows.yml -f tag="$tag"
        for _ in $(seq 30); do
            run_id="$(find_run)"
            [ -n "$run_id" ] && break
            sleep 2
        done
        [ -n "$run_id" ] || { echo "the Windows run did not show up" >&2; exit 1; }
    fi
    echo "run $run_id"
fi

say "Linux: build on the baseline, smoke, verify, publish"
# The build of THIS commit of the tag: one of a tag since moved has the
# same `<tag>-0-g` prefix and another hash.
tag_sha="$(git rev-parse "$tag^{commit}")"
# `if`, not `&&`: under `set -e` and `pipefail`, a last directory that does
# not match would fail the pipeline and kill the script without a word.
this_build() {
    local d found=""
    for d in target/baseline/"$tag"-0-g*; do
        if [ -d "$d" ] && [[ "$tag_sha" == "${d##*-0-g}"* ]]; then found="$d"; fi
    done
    echo "$found"
}
dir="$(this_build)"
if [ -z "$dir" ] || ! scripts/baseline/verify.sh "$dir" > /dev/null 2>&1; then
    just baseline "$tag"
    dir="$(this_build)"
    [ -n "$dir" ] || { echo "no baseline build of $tag at $tag_sha" >&2; exit 1; }
fi
# Published = EVERY file the checksums name is there, not just the sums: an
# upload cut halfway leaves some.
published="$(assets)"
missing=0
while read -r _ name; do
    # `*dist/x.tar.xz` in the sums, `x.tar.xz` among the assets.
    name="${name#\*}"
    grep -qxF "${name##*/}" <<< "$published" || missing=1
done < "$dir/SHA256SUMS"
if [ "$missing" = 1 ] || ! grep -qxF SHA256SUMS <<< "$published"; then
    must_be_draft
    just baseline-publish "$tag" "$dir"
else
    echo "already published"
fi

say "disk-compat corpus, from the published norte"
if [ -d "tests/disk-compat/$version" ]; then
    echo "already frozen"
else
    tar -xJf "$dir/dist/norte-cli-x86_64-unknown-linux-gnu.tar.xz" -C "$tmp"
    bin="$(find "$tmp" -type f -name norte -perm -u+x | head -1)"
    [ -n "$bin" ] || { echo "no norte binary in the CLI tarball" >&2; exit 1; }
    scripts/release/freeze-disk-compat.sh "$version" "$bin"
fi

if [ -n "$run_id" ]; then
    say "Windows: waiting for run $run_id"
    gh run watch "$run_id" --exit-status > /dev/null
    mkdir -p "$tmp/win"
    gh run download "$run_id" -n "windows-unsigned-$tag" -D "$tmp/win"
    mv "$tmp/win/SHA256SUMS" "$tmp/win/SHA256SUMS-windows"
    must_be_draft
    gh release upload "$tag" "$tmp/win"/* --clobber
fi

say "done: $tag is a DRAFT with every artefact"
echo "read it:    gh release view $tag --web"
echo "publish:    gh release edit $tag --draft=false"
echo "commit:     tests/disk-compat/$version (see the freeze's 'next' lines above)"
