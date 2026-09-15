#!/usr/bin/env bash
# Construye la imagen de construcción si su tag no existe y escribe el tag.
# El contexto de Docker son TRES ficheros, no el repo: el repo tiene un
# `target/` de decenas de GB que Docker copiaría entero.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

tag="$(builder_tag "$RAIZ")"
if ! docker image inspect "$tag" >/dev/null 2>&1; then
  echo "construyendo $tag" >&2
  tar -c -C "$RAIZ/scripts/baseline" Dockerfile -C "$RAIZ" rust-toolchain.toml dist-workspace.toml |
    docker build -t "$tag" - >&2
fi
printf '%s\n' "$tag"
