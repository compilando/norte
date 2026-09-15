#!/usr/bin/env bash
# Pruebas de `lib.sh`: sin Docker ni red, en milisegundos. Lo que se puede
# equivocar en silencio —el orden de versiones, un suelo que deja pasar un
# binario, una suma que no se comprueba— se prueba aquí y no en una build de
# veinte minutos.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

fallos=0
igual() {
  if [ "$2" = "$3" ]; then
    echo "ok   $1"
  else
    echo "FALLA $1: se esperaba '$3', fue '$2'"
    fallos=$((fallos + 1))
  fi
}
cierto() { if "${@:2}"; then igual "$1" si si; else igual "$1" no si; fi; }
falso() { if "${@:2}"; then igual "$1" si no; else igual "$1" no no; fi; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# glibc_max
igual "glibc_max toma la más alta" \
  "$(printf '0 GLIBC_2.17 x\n0 GLIBC_2.34 y\n0 GLIBC_2.3.4 z\n' | glibc_max)" "2.34"
igual "glibc_max sin símbolos no dice nada" "$(printf 'nada\n' | glibc_max)" ""

# version_le
cierto "2.34 <= 2.35" version_le 2.34 2.35
cierto "2.35 <= 2.35" version_le 2.35 2.35
cierto "2.3.4 <= 2.35" version_le 2.3.4 2.35
falso "2.39 <= 2.35" version_le 2.39 2.35

# version_matches
cierto "versión con la revisión" version_matches "norte 0.3.0-alpha.4 (v0.3.0-alpha.4-0-gabc)" "v0.3.0-alpha.4-0-gabc"
falso "versión de otra revisión" version_matches "norte 0.3.0-alpha.4 (v0.3.0-alpha.4-1-gdef)" "v0.3.0-alpha.4-0-gabc"
falso "revisión unknown" version_matches "norte 0.3.0-alpha.4 (unknown)" "v0.3.0-alpha.4-0-gabc"

# image_slug
igual "slug con tag" "$(image_slug ubuntu:22.04)" "ubuntu-22.04"
igual "slug con tag y digest" "$(image_slug debian:trixie@sha256:f324)" "debian-trixie"
igual "slug solo digest" "$(image_slug debian@sha256:f324)" "debian"

# manifest_sums / manifest_check_sums
mkdir -p "$tmp/out/dist" "$tmp/out/smoke"
echo a >"$tmp/out/dist/a.tar.xz"
echo b >"$tmp/out/b.deb"
echo m >"$tmp/out/MANIFEST"
echo s >"$tmp/out/smoke/x.log"
manifest_sums "$tmp/out"
igual "SHA256SUMS lista dos ficheros" "$(wc -l <"$tmp/out/SHA256SUMS" | tr -d ' ')" "2"
igual "SHA256SUMS en formato dist" "$(grep -c ' \*dist/a.tar.xz$' "$tmp/out/SHA256SUMS")" "1"
cierto "sumas intactas cuadran" manifest_check_sums "$tmp/out"
echo cambiado >"$tmp/out/b.deb"
falso "una suma tocada no cuadra" manifest_check_sums "$tmp/out"

# manifest_problems
cat >"$tmp/bueno" <<'EOF'
revision v1-0-gabc
glibc-floor 2.35
glibc deb/usr/bin/norte-gui 2.34
glibc deb/usr/bin/norte 2.34
glibc tar/norte-tui/ntc none
version deb/usr/bin/norte norte 1 (v1-0-gabc)
EOF
igual "manifest bueno sin problemas" "$(manifest_problems "$tmp/bueno")" ""
cat >"$tmp/malo" <<'EOF'
revision v1-0-gabc
glibc-floor 2.35
glibc tar/x/norte 2.39
version tar/x/norte norte 1 (v1-1-gdef)
EOF
# suelo superado, revisión ajena, y faltan ntc y norte-gui: cuatro.
igual "manifest malo: cuatro problemas" "$(manifest_problems "$tmp/malo" | wc -l | tr -d ' ')" "4"
printf 'revision v1\n' >"$tmp/sinsuelo"
igual "sin suelo es un problema" "$(manifest_problems "$tmp/sinsuelo" | grep -c 'glibc-floor')" "1"

# smoke_missing
printf '# comentario\n\ntarball ubuntu:22.04@sha256:1\ndeb debian:bookworm@sha256:2\n' >"$tmp/matrix"
printf 'ok tarball ubuntu:22.04@sha256:1\n' >"$tmp/smoke"
igual "falta un humo" "$(smoke_missing "$tmp/matrix" "$tmp/smoke")" "deb debian:bookworm@sha256:2"
igual "sin fichero de humos faltan todos" "$(smoke_missing "$tmp/matrix" "$tmp/no-existe" | wc -l | tr -d ' ')" "2"

# builder_tag
mkdir -p "$tmp/r/scripts/baseline"
echo 'FROM x' >"$tmp/r/scripts/baseline/Dockerfile"
echo 'channel = "1"' >"$tmp/r/rust-toolchain.toml"
echo 'cargo-dist-version = "0"' >"$tmp/r/dist-workspace.toml"
t1="$(builder_tag "$tmp/r")"
echo 'FROM y' >"$tmp/r/scripts/baseline/Dockerfile"
t2="$(builder_tag "$tmp/r")"
cierto "tag con forma norte-builder:hex12" bash -c "[[ '$t1' =~ ^norte-builder:[0-9a-f]{12}\$ ]]"
falso "cambiar el Dockerfile cambia el tag" test "$t1" = "$t2"

if [ "$fallos" -gt 0 ]; then
  echo "$fallos fallos"
  exit 1
fi
echo "selftest: todo en orden"
