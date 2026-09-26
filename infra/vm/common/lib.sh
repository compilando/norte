#!/usr/bin/env bash
# Shared, side-effect-free helpers for local VM orchestration.
set -euo pipefail

vm_repo_root() {
  cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd
}

vm_lab_home() {
  if [ -n "${NORTE_LAB_HOME:-}" ]; then
    realpath -m "$NORTE_LAB_HOME"
  else
    realpath -m "$(vm_repo_root)/../norte-lab"
  fi
}

vm_require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing command: $1" >&2
    return 1
  }
}

vm_check_sha256() {
  local file="$1" expected="$2" actual
  [ -f "$file" ] || { echo "file does not exist: $file" >&2; return 1; }
  [[ "$expected" =~ ^[0-9a-fA-F]{64}$ ]] || {
    echo "invalid SHA-256 value for $file" >&2
    return 1
  }
  actual="$(sha256sum "$file" | awk '{print $1}')"
  [ "${actual,,}" = "${expected,,}" ] || {
    echo "SHA-256 does not match for $file" >&2
    echo "expected: ${expected,,}" >&2
    echo "actual:   ${actual,,}" >&2
    return 1
  }
}

vm_assert_external_state_path() {
  local root repo
  root="$(realpath -m "$1")"
  repo="$(vm_repo_root)"
  case "$root" in
    /|/home|"$HOME"|"$repo"|"$repo"/*)
      echo "refusing unsafe VM state root: $root" >&2
      return 1
      ;;
  esac
}
