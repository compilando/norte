#!/usr/bin/env bash
# Builds the build image if its tag does not exist, and prints the tag.
# Docker's context is THREE files, not the repo: the repo has a `target/` of
# tens of GB that Docker would copy whole.
set -euo pipefail
RAIZ="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=scripts/baseline/lib.sh
source "$RAIZ/scripts/baseline/lib.sh"

tag="$(builder_tag "$RAIZ")"
if ! docker image inspect "$tag" >/dev/null 2>&1; then
  echo "building $tag" >&2
  tar -c -C "$RAIZ/scripts/baseline" Dockerfile -C "$RAIZ" rust-toolchain.toml dist-workspace.toml |
    docker build -t "$tag" - >&2
fi
printf '%s\n' "$tag"
