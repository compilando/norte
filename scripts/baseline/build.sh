#!/usr/bin/env bash
# Builds EVERYTHING that would be published from a ref —`dist`'s tarballs and
# installers, and the window's deb/rpm/AppImage— inside the build image, and
# leaves a MANIFEST saying which glibc each binary requires and which
# revision it claims to be (ADR 0112).
#
#   scripts/baseline/build.sh [ref]      # HEAD by default
#
# The repo comes in via a clone INSIDE the container from the `.git` mounted
# read-only: the revision (`git describe`) comes from the requested ref and
# not from HEAD, and `dist` can make its `source.tar.gz`. The host's
# `target/` is not touched.
#
# All progress goes to stderr; stdout is ONE line, the output directory.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

REF="${1:-HEAD}"
FLOOR="${NORTE_GLIBC_FLOOR:-2.35}"

for c in docker git bsdtar objdump; do
  command -v "$c" >/dev/null || { echo "need $c" >&2; exit 1; }
done

commit="$(git -C "$RAIZ" rev-parse --verify "$REF^{commit}")"
revision="$(git -C "$RAIZ" describe --tags --always --long "$commit")"
gitdir="$(git -C "$RAIZ" rev-parse --path-format=absolute --git-common-dir)"
SALIDA="$RAIZ/target/baseline/$revision"
imagen="$("$RAIZ/scripts/baseline/image.sh")"

echo "ref $REF · commit ${commit:0:12} · revision $revision · $imagen" >&2
rm -rf "$SALIDA"
mkdir -p "$SALIDA"

# shellcheck disable=SC2016  # expands INSIDE the container
DENTRO='
set -euo pipefail
export CARGO_TARGET_DIR=/target
git clone -q /repo.git /src
cd /src
git checkout -q "$COMMIT"
triple="$(rustc -vV | grep "^host:" | cut -d" " -f2)"
rm -rf /target/distrib /src/target/distrib /target/release/bundle

echo "--- dist: norte and ntc"
dist build --artifacts=local --target="$triple"
dist build --artifacts=global --target="$triple"

echo "--- webview"
(cd crates/norte-gui-tauri/ui && npm ci --no-audit --no-fund && npm run build)

echo "--- window package, with the SAME norte and ntc as the tarballs"
mkdir -p crates/norte-gui-tauri/binaries
for b in norte ntc; do
  src="$(find /target -type f -perm -u+x -path "*/dist/$b" -print -quit)"
  [ -n "$src" ] || { echo "dist did not leave binary $b under /target" >&2; exit 1; }
  cp "$src" "crates/norte-gui-tauri/binaries/$b-$triple"
done
(cd crates/norte-gui-tauri && NO_STRIP=1 APPIMAGE_EXTRACT_AND_RUN=1 ./ui/node_modules/.bin/tauri build)

mkdir -p /out/dist /out/gui
distrib=/target/distrib
[ -d "$distrib" ] || distrib=/src/target/distrib
find "$distrib" -maxdepth 1 -type f -exec cp {} /out/dist/ \;
find /target/release/bundle -maxdepth 2 -type f \( -name "*.deb" -o -name "*.rpm" -o -name "*.AppImage" \) -exec cp {} /out/gui/ \;
chown -R "$HOST_UID:$HOST_GID" /out
'

docker run --rm \
  -v "$gitdir:/repo.git:ro" \
  -v norte-baseline-registry:/opt/cargo/registry \
  -v norte-baseline-target:/target \
  -v "$SALIDA:/out" \
  -e COMMIT="$commit" \
  -e HOST_UID="$(id -u)" \
  -e HOST_GID="$(id -g)" \
  "$imagen" bash -c "$DENTRO" >&2

# --- MANIFEST: what would be published is opened and looked at INSIDE ----
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/tar" "$tmp/deb"
for t in "$SALIDA"/dist/*.tar.xz; do tar -xf "$t" -C "$tmp/tar"; done
deb="$(find "$SALIDA/gui" -name '*.deb' -print -quit)"
[ -n "$deb" ] || { echo "the build did not leave a .deb" >&2; exit 1; }
bsdtar -xOf "$deb" 'data.tar.*' | bsdtar -x -C "$tmp/deb"

{
  echo "ref $REF"
  echo "commit $commit"
  echo "revision $revision"
  echo "glibc-floor $FLOOR"
  echo "builder $imagen"
  while IFS= read -r bin; do
    rel="${bin#"$tmp"/}"
    v="$(objdump -T "$bin" | glibc_max)"
    echo "glibc $rel ${v:-none}"
    case "${bin##*/}" in
      norte | ntc) echo "version $rel $("$bin" --version)" ;;
    esac
  done < <(find "$tmp" -type f \( -name norte -o -name ntc -o -name norte-gui \) | LC_ALL=C sort)
} >"$SALIDA/MANIFEST"

manifest_sums "$SALIDA"

problemas="$(manifest_problems "$SALIDA/MANIFEST")"
if [ -n "$problemas" ]; then
  printf 'the build is NOT valid:\n%s\n' "$problemas" >&2
  exit 1
fi
printf '%s\n' "$SALIDA"
