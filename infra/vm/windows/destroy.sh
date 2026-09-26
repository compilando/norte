#!/usr/bin/env bash
# Remove the libvirt definition. The qcow2 is retained for explicit recovery.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ ! -f "$HERE/config.env" ] || source "$HERE/config.env"
name="${NORTE_WINDOWS_VM_NAME:-norte-win11-build}"
[[ "$name" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid VM name: $name" >&2; exit 1; }
virsh --connect qemu:///system destroy "$name" 2>/dev/null || true
virsh --connect qemu:///system undefine "$name" --nvram --snapshots-metadata
echo "undefined $name; its external qcow2 was NOT deleted"
