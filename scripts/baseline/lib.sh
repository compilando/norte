# shellcheck shell=bash
# Funciones puras del sistema de base (ADR 0112): sin Docker, sin red, sin
# estado. Las prueba `selftest.sh`; las usan `build.sh`, `smoke.sh` y
# `verify.sh`.

# La versión GLIBC_ más alta de una salida de `objdump -T` (stdin), sin
# prefijo. Nada si no hay ninguna. Siempre sale 0: un binario sin símbolos de
# glibc no es un error de esta función.
glibc_max() {
  { grep -oE 'GLIBC_[0-9]+\.[0-9]+(\.[0-9]+)?' || true; } |
    sed 's/^GLIBC_//' | sort -Vu | tail -n 1
}

# 0 si $1 <= $2 en orden de versiones (2.3.4 < 2.35).
version_le() {
  [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n 1)" = "$1" ]
}

# 0 si la salida de `--version` ($1) termina en «($2)».
version_matches() {
  [[ "$1" == *"($2)" ]]
}

# `debian:trixie@sha256:…` → `debian-trixie`: un nombre de fichero por imagen.
image_slug() {
  local s="${1%%@*}"
  printf '%s\n' "${s//[:\/]/-}"
}

# SHA256SUMS de todo fichero bajo $1 salvo los que se escriben DESPUÉS (las
# sumas mismas, el MANIFEST, los humos). Formato de `dist`: `<hash> *<ruta>`.
manifest_sums() {
  local dir="$1"
  (
    cd "$dir" || exit 1
    find . -type f ! -name SHA256SUMS ! -name MANIFEST ! -name SMOKE ! -path './smoke/*' -printf '%P\n' |
      LC_ALL=C sort |
      while IFS= read -r f; do sha256sum --binary -- "$f"; done
  ) >"$dir/SHA256SUMS"
}

# 0 si toda suma de $1/SHA256SUMS cuadra.
manifest_check_sums() {
  (cd "$1" && sha256sum --quiet --strict -c SHA256SUMS)
}

# Un problema por línea; nada si el MANIFEST es aceptable.
manifest_problems() {
  local file="$1" floor revision kind path rest b
  floor="$(awk '$1 == "glibc-floor" { print $2 }' "$file")"
  revision="$(awk '$1 == "revision" { print $2 }' "$file")"
  [ -n "$floor" ] || echo "MANIFEST sin glibc-floor"
  [ -n "$revision" ] || echo "MANIFEST sin revision"
  for b in norte ntc norte-gui; do
    grep -qE "^glibc [^ ]*/$b " "$file" || echo "MANIFEST sin glibc de $b"
  done
  while read -r kind path rest; do
    case "$kind" in
      glibc)
        if [ "$rest" != none ] && [ -n "$floor" ] && ! version_le "$rest" "$floor"; then
          echo "$path pide glibc $rest, por encima del suelo $floor"
        fi
        ;;
      version)
        version_matches "$rest" "$revision" || echo "$path dice «$rest», se esperaba ($revision)"
        ;;
    esac
  done <"$file"
}

# Las líneas `artefacto imagen` de la matriz sin su `ok artefacto imagen`.
smoke_missing() {
  local matrix="$1" record="$2" artifact image
  while read -r artifact image; do
    case "${artifact:-}" in '' | \#*) continue ;; esac
    grep -qxF "ok $artifact $image" "$record" 2>/dev/null || printf '%s %s\n' "$artifact" "$image"
  done <"$matrix"
}

# El tag de la imagen de construcción: cambia si cambia cualquiera de sus
# entradas, así que una imagen vieja no se reusa por error.
builder_tag() {
  local root="$1"
  printf 'norte-builder:%s\n' "$(cat "$root/scripts/baseline/Dockerfile" \
    "$root/rust-toolchain.toml" "$root/dist-workspace.toml" | sha256sum | cut -c1-12)"
}
