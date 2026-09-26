#!/usr/bin/env bash
# Attach a read-only ISO containing the guest-side Windows scripts.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
# shellcheck source=infra/vm/common/lib.sh
source "$ROOT/infra/vm/common/lib.sh"
[ ! -f "$HERE/config.env" ] || source "$HERE/config.env"

name="${NORTE_WINDOWS_VM_NAME:-norte-win11-build}"
lab="$(vm_lab_home)/windows"
vm_assert_external_state_path "$lab"
mkdir -p "$lab/generated"
iso="$lab/generated/norte-bootstrap.iso"
xorriso -as mkisofs -quiet -J -r -V NORTEBOOT -o "$iso" \
  "$ROOT/scripts/platform/windows/bootstrap.ps1" \
  "$ROOT/scripts/platform/windows/check.ps1"
setfacl -m "u:${NORTE_LIBVIRT_QEMU_USER:-libvirt-qemu}:r" "$iso"

if virsh --connect qemu:///system dumpxml "$name" | grep -q "source file='$iso'"; then
  echo "bootstrap ISO already attached: $iso"
  exit 0
fi
virsh --connect qemu:///system attach-disk "$name" "$iso" sdd \
  --type cdrom --targetbus sata --mode readonly --config
echo "attached $iso as the next Windows CD-ROM; restart the VM to expose it"
