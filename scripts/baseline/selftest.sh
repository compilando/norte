#!/usr/bin/env bash
# Tests for `lib.sh`: no Docker, no network, in milliseconds. What can go
# wrong silently —version ordering, a floor that lets a binary through, a
# checksum that is never checked— is tested here and not in a twenty-minute
# build.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

fallos=0
igual() {
  if [ "$2" = "$3" ]; then
    echo "ok   $1"
  else
    echo "FAIL $1: expected '$3', got '$2'"
    fallos=$((fallos + 1))
  fi
}
cierto() { if "${@:2}"; then igual "$1" yes yes; else igual "$1" no yes; fi; }
falso() { if "${@:2}"; then igual "$1" yes no; else igual "$1" no no; fi; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# glibc_max
igual "glibc_max takes the highest one" \
  "$(printf '0 GLIBC_2.17 x\n0 GLIBC_2.34 y\n0 GLIBC_2.3.4 z\n' | glibc_max)" "2.34"
igual "glibc_max with no symbols says nothing" "$(printf 'nothing\n' | glibc_max)" ""

# version_le
cierto "2.34 <= 2.35" version_le 2.34 2.35
cierto "2.35 <= 2.35" version_le 2.35 2.35
cierto "2.3.4 <= 2.35" version_le 2.3.4 2.35
falso "2.39 <= 2.35" version_le 2.39 2.35

# version_matches
cierto "version with the revision" version_matches "norte 0.3.0-alpha.4 (v0.3.0-alpha.4-0-gabc)" "v0.3.0-alpha.4-0-gabc"
falso "version from another revision" version_matches "norte 0.3.0-alpha.4 (v0.3.0-alpha.4-1-gdef)" "v0.3.0-alpha.4-0-gabc"
falso "unknown revision" version_matches "norte 0.3.0-alpha.4 (unknown)" "v0.3.0-alpha.4-0-gabc"

# image_slug
igual "slug with tag" "$(image_slug ubuntu:22.04)" "ubuntu-22.04"
igual "slug with tag and digest" "$(image_slug debian:trixie@sha256:f324)" "debian-trixie"
igual "slug with only a digest" "$(image_slug debian@sha256:f324)" "debian"

# manifest_sums / manifest_check_sums
mkdir -p "$tmp/out/dist" "$tmp/out/smoke"
echo a >"$tmp/out/dist/a.tar.xz"
echo b >"$tmp/out/b.deb"
echo m >"$tmp/out/MANIFEST"
echo s >"$tmp/out/smoke/x.log"
manifest_sums "$tmp/out"
igual "SHA256SUMS lists two files" "$(wc -l <"$tmp/out/SHA256SUMS" | tr -d ' ')" "2"
igual "SHA256SUMS in dist format" "$(grep -c ' \*dist/a.tar.xz$' "$tmp/out/SHA256SUMS")" "1"
cierto "untouched sums match" manifest_check_sums "$tmp/out"
echo changed >"$tmp/out/b.deb"
falso "a touched sum does not match" manifest_check_sums "$tmp/out"

# manifest_problems
cat >"$tmp/bueno" <<'EOF'
revision v1-0-gabc
glibc-floor 2.35
glibc deb/usr/bin/norte-gui 2.34
glibc deb/usr/bin/norte 2.34
glibc tar/norte-tui/ntc none
version deb/usr/bin/norte norte 1 (v1-0-gabc)
EOF
igual "good manifest has no problems" "$(manifest_problems "$tmp/bueno")" ""
cat >"$tmp/malo" <<'EOF'
revision v1-0-gabc
glibc-floor 2.35
glibc tar/x/norte 2.39
version tar/x/norte norte 1 (v1-1-gdef)
EOF
# floor exceeded, foreign revision, and ntc and norte-gui are missing: four.
igual "bad manifest: four problems" "$(manifest_problems "$tmp/malo" | wc -l | tr -d ' ')" "4"
printf 'revision v1\n' >"$tmp/sinsuelo"
igual "no floor is a problem" "$(manifest_problems "$tmp/sinsuelo" | grep -c 'glibc-floor')" "1"

# smoke_missing
printf '# comment\n\ntarball ubuntu:22.04@sha256:1\ndeb debian:bookworm@sha256:2\n' >"$tmp/matrix"
printf 'ok tarball ubuntu:22.04@sha256:1\n' >"$tmp/smoke"
igual "one smoke test missing" "$(smoke_missing "$tmp/matrix" "$tmp/smoke")" "deb debian:bookworm@sha256:2"
igual "with no smoke file all are missing" "$(smoke_missing "$tmp/matrix" "$tmp/no-existe" | wc -l | tr -d ' ')" "2"

# builder_tag
mkdir -p "$tmp/r/scripts/baseline"
echo 'FROM x' >"$tmp/r/scripts/baseline/Dockerfile"
echo 'channel = "1"' >"$tmp/r/rust-toolchain.toml"
echo 'cargo-dist-version = "0"' >"$tmp/r/dist-workspace.toml"
t1="$(builder_tag "$tmp/r")"
echo 'FROM y' >"$tmp/r/scripts/baseline/Dockerfile"
t2="$(builder_tag "$tmp/r")"
cierto "tag has the shape norte-builder:hex12" bash -c "[[ '$t1' =~ ^norte-builder:[0-9a-f]{12}\$ ]]"
falso "changing the Dockerfile changes the tag" test "$t1" = "$t2"

if [ "$fallos" -gt 0 ]; then
  echo "$fallos failures"
  exit 1
fi
echo "selftest: all good"
