#!/usr/bin/env bash
# Read-only host and input validation for the Windows build VM.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
# shellcheck source=infra/vm/common/lib.sh
source "$ROOT/infra/vm/common/lib.sh"
[ ! -f "$HERE/config.env" ] || source "$HERE/config.env"

failed=0
check() {
  if "$@"; then
    printf 'ok   %s\n' "$*"
  else
    printf 'FAIL %s\n' "$*" >&2
    failed=$((failed + 1))
  fi
}

for command in virsh virt-install qemu-img swtpm xorriso sha256sum openssl setfacl ssh scp; do
  check vm_require_command "$command"
done

qemu_user="${NORTE_LIBVIRT_QEMU_USER:-libvirt-qemu}"
if getent passwd "$qemu_user" >/dev/null; then
  echo "ok   libvirt QEMU user $qemu_user"
else
  echo "FAIL libvirt QEMU user does not exist: $qemu_user" >&2
  echo "     set NORTE_LIBVIRT_QEMU_USER for this distribution" >&2
  failed=$((failed + 1))
fi

lab="$(vm_lab_home)"
check vm_assert_external_state_path "$lab"

if [ -c /dev/kvm ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then
  echo "ok   /dev/kvm is usable"
else
  echo "FAIL /dev/kvm is not usable by $(id -un)" >&2
  failed=$((failed + 1))
fi

if virsh --connect qemu:///system list --all >/dev/null 2>&1; then
  echo "ok   libvirt system connection"
else
  echo "FAIL cannot connect to qemu:///system" >&2
  echo "     enable libvirt and add $(id -un) to its access group" >&2
  failed=$((failed + 1))
fi

network_info="$(LC_ALL=C virsh --connect qemu:///system net-info default 2>/dev/null || true)"
if grep -q '^Active:[[:space:]]*yes$' <<<"$network_info"; then
  echo "ok   libvirt default NAT network"
else
  echo "FAIL libvirt's default NAT network is not active" >&2
  echo "     run: virsh --connect qemu:///system net-start default" >&2
  failed=$((failed + 1))
fi

if [ -z "${NORTE_WINDOWS_ISO:-}" ] || [ -z "${NORTE_WINDOWS_ISO_SHA256:-}" ]; then
  echo "FAIL copy config.example.env to config.env and set the ISO path and digest" >&2
  failed=$((failed + 1))
else
  check vm_check_sha256 "$NORTE_WINDOWS_ISO" "$NORTE_WINDOWS_ISO_SHA256"
fi

printf '\nstate root: %s\n' "$lab"
if [ "$failed" -gt 0 ]; then
  echo "preflight: $failed problem(s)" >&2
  exit 1
fi
echo "preflight: ready"
