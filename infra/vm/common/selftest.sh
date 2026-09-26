#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
# shellcheck source=infra/vm/common/lib.sh
source "$ROOT/infra/vm/common/lib.sh"

failures=0
equal() {
  if [ "$2" = "$3" ]; then
    echo "ok   $1"
  else
    echo "FAIL $1: expected '$3', got '$2'"
    failures=$((failures + 1))
  fi
}
true_case() { if "${@:2}"; then equal "$1" yes yes; else equal "$1" no yes; fi; }
false_case() { if "${@:2}"; then equal "$1" yes no; else equal "$1" no no; fi; }

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

expected_default="$(realpath -m "$ROOT/../norte-lab")"
equal "default state is beside the repository" "$(vm_lab_home)" "$expected_default"
NORTE_LAB_HOME="$tmp/lab"
export NORTE_LAB_HOME
equal "state root can be overridden" "$(vm_lab_home)" "$(realpath -m "$tmp/lab")"

printf test >"$tmp/file"
digest="$(sha256sum "$tmp/file" | awk '{print $1}')"
true_case "matching digest is accepted" vm_check_sha256 "$tmp/file" "$digest"
false_case "wrong digest is rejected" vm_check_sha256 "$tmp/file" "$(printf '0%.0s' {1..64})"
false_case "short digest is rejected" vm_check_sha256 "$tmp/file" deadbeef

true_case "external temporary state is accepted" vm_assert_external_state_path "$tmp/lab"
false_case "repository cannot be VM state" vm_assert_external_state_path "$ROOT"
false_case "repository child cannot be VM state" vm_assert_external_state_path "$ROOT/target/vm"
false_case "home cannot be VM state" vm_assert_external_state_path "$HOME"

if [ "$failures" -gt 0 ]; then
  echo "$failures failures" >&2
  exit 1
fi
echo "vm selftest: all good"
