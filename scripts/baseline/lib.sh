# shellcheck shell=bash
# Pure functions for the baseline system (ADR 0112): no Docker, no network,
# no state. Tested by `selftest.sh`; used by `build.sh`, `smoke.sh` and
# `verify.sh`.

# The highest GLIBC_ version from an `objdump -T` output (stdin), without the
# prefix. Nothing if there is none. Always exits 0: a binary with no glibc
# symbols is not an error for this function.
glibc_max() {
  { grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' || true; } |
    sed 's/^GLIBC_//' | sort -Vu | tail -n 1
}

# 0 if $1 <= $2 in version order (2.3.4 < 2.35).
version_le() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$1" ]
}

# 0 if `--version`'s output ($1) ends in "($2)".
version_matches() {
  [[ "$1" == *"($2)" ]]
}

# `debian:trixie@sha256:…` → `debian-trixie`: one file name per image.
image_slug() {
  local s="${1%%@*}"
  printf '%s\n' "${s//[:\/]/-}"
}

# SHA256SUMS of every file under $1 except the ones written AFTERWARD (the
# sums themselves, the MANIFEST, the smoke tests). `dist`'s format:
# `<hash> *<path>`.
manifest_sums() {
  local dir="$1"
  (
    cd "$dir" || exit 1
    find . -type f ! -name SHA256SUMS ! -name MANIFEST ! -name SMOKE ! -path './smoke/*' -printf '%P\n' |
      LC_ALL=C sort |
      while IFS= read -r f; do sha256sum --binary -- "$f"; done
  ) >"$dir/SHA256SUMS"
}

# 0 if every sum in $1/SHA256SUMS matches.
manifest_check_sums() {
  (cd "$1" && sha256sum --quiet --strict -c SHA256SUMS)
}

# One problem per line; nothing if the MANIFEST is acceptable.
manifest_problems() {
  local file="$1" floor revision kind path rest b
  floor="$(awk '$1 == "glibc-floor" { print $2 }' "$file")"
  revision="$(awk '$1 == "revision" { print $2 }' "$file")"
  [ -n "$floor" ] || echo "MANIFEST with no glibc-floor"
  [ -n "$revision" ] || echo "MANIFEST with no revision"
  for b in norte ntc norte-gui; do
    grep -qE "^glibc [^ ]*/$b " "$file" || echo "MANIFEST with no glibc for $b"
  done
  while read -r kind path rest; do
    case "$kind" in
      glibc)
        if [ "$rest" != none ] && [ -n "$floor" ] && ! version_le "$rest" "$floor"; then
          echo "$path requires glibc $rest, above the $floor floor"
        fi
        ;;
      version)
        version_matches "$rest" "$revision" || echo "$path says «$rest», expected ($revision)"
        ;;
    esac
  done <"$file"
}

# The matrix's `artifact image` lines without their `ok artifact image`.
smoke_missing() {
  local matrix="$1" record="$2" artifact image
  while read -r artifact image; do
    case "${artifact:-}" in '' | \#*) continue ;; esac
    grep -qxF "ok $artifact $image" "$record" 2>/dev/null || printf '%s %s\n' "$artifact" "$image"
  done <"$matrix"
}

# The build image's tag: changes if any of its inputs change, so an old
# image is not reused by mistake.
builder_tag() {
  local root="$1"
  printf 'norte-builder:%s\n' "$(cat "$root/scripts/baseline/Dockerfile" \
    "$root/rust-toolchain.toml" "$root/dist-workspace.toml" | sha256sum | cut -c1-12)"
}
