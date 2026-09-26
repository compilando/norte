#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ ! -f "$HERE/config.env" ] || source "$HERE/config.env"
name="${NORTE_WINDOWS_VM_NAME:-norte-win11-build}"
snapshot="${1:-provisioned}"
[[ "$name" =~ ^[A-Za-z0-9._-]+$ && "$snapshot" =~ ^[A-Za-z0-9._-]+$ ]] || {
  echo "invalid VM or snapshot name" >&2
  exit 1
}

connection=qemu:///system
disk="$(virsh --connect "$connection" domblklist "$name" --details \
  | awk '$2 == "disk" && !found { print $4; found=1 }')"
[ -n "$disk" ] || {
  echo "could not find the VM's SATA system disk" >&2
  exit 1
}

was_running=false
restart_if_needed() {
  if $was_running && [ "$(virsh --connect "$connection" domstate "$name")" = "shut off" ]; then
    virsh --connect "$connection" start "$name" >/dev/null
  fi
}
trap restart_if_needed EXIT
if [ "$(virsh --connect "$connection" domstate "$name")" = running ]; then
  was_running=true
  virsh --connect "$connection" shutdown "$name"
  for _ in $(seq 1 60); do
    [ "$(virsh --connect "$connection" domstate "$name")" = "shut off" ] && break
    sleep 2
  done
fi
[ "$(virsh --connect "$connection" domstate "$name")" = "shut off" ] || {
  echo "VM did not shut down; snapshot was not created" >&2
  exit 1
}
[ "$(qemu-img info --output=json "$disk" | sed -n 's/.*"format": "\([^"]*\)".*/\1/p' | tail -n 1)" = qcow2 ] || {
  echo "system disk is not QCOW2: $disk" >&2
  exit 1
}

# Snapshot the system disk directly. A libvirt internal snapshot also includes
# UEFI pflash, whose raw NVRAM format cannot be snapshotted on many hosts.
qemu-img snapshot -c "$snapshot" "$disk"
restart_if_needed
was_running=false
echo "created disk snapshot '$snapshot' for $name"
