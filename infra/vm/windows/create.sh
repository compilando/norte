#!/usr/bin/env bash
# Create the disposable Windows development/build VM from verified media.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
# shellcheck source=infra/vm/common/lib.sh
source "$ROOT/infra/vm/common/lib.sh"
[ ! -f "$HERE/config.env" ] || source "$HERE/config.env"

"$HERE/preflight.sh"

name="${NORTE_WINDOWS_VM_NAME:-norte-win11-build}"
memory="${NORTE_WINDOWS_MEMORY_MIB:-16384}"
vcpus="${NORTE_WINDOWS_VCPUS:-8}"
disk_gib="${NORTE_WINDOWS_DISK_GIB:-160}"
user="${NORTE_WINDOWS_USER:-norte}"
image_name="${NORTE_WINDOWS_IMAGE_NAME:-Windows 11 Pro}"
locale="${NORTE_WINDOWS_LOCALE:-en-US}"
product_key="${NORTE_WINDOWS_PRODUCT_KEY:-W269N-WFGWX-YVC9B-4J6C9-T83GX}"
qemu_user="${NORTE_LIBVIRT_QEMU_USER:-libvirt-qemu}"
lab="$(vm_lab_home)/windows"
vm_assert_external_state_path "$lab"
mkdir -p "$lab/disks" "$lab/generated" "$lab/artifacts" "$lab/secrets"

password_file="${NORTE_WINDOWS_PASSWORD_FILE:-$lab/secrets/windows-password}"
if [ -z "${NORTE_WINDOWS_PASSWORD:-}" ]; then
  if [ ! -f "$password_file" ]; then
    (umask 077; openssl rand -hex 16 >"$password_file")
  fi
  NORTE_WINDOWS_PASSWORD="$(<"$password_file")"
fi
chmod 600 "$password_file" 2>/dev/null || true
[[ "$name" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid VM name: $name" >&2; exit 1; }
[[ "$user" =~ ^[A-Za-z0-9._-]+$ ]] || { echo "invalid Windows user: $user" >&2; exit 1; }
[[ "$memory" =~ ^[0-9]+$ && "$vcpus" =~ ^[0-9]+$ && "$disk_gib" =~ ^[0-9]+$ ]] || {
  echo "memory, vcpus and disk size must be positive integers" >&2
  exit 1
}
# The value is rendered into XML and then used for password authentication
# during bootstrap. Keep its alphabet deliberately boring: no XML/sed
# metacharacters, shell whitespace or accidental newlines in generated media.
[[ "$NORTE_WINDOWS_PASSWORD" =~ ^[A-Za-z0-9._@%+=:-]{12,}$ ]] || {
  echo "NORTE_WINDOWS_PASSWORD must be at least 12 safe ASCII characters" >&2
  exit 1
}
[[ "$image_name" =~ ^[A-Za-z0-9._()[:space:]-]+$ ]] || {
  echo "invalid Windows image name: $image_name" >&2
  exit 1
}
[[ "$locale" =~ ^[a-z]{2}-[A-Z]{2}$ ]] || {
  echo "invalid Windows locale: $locale" >&2
  exit 1
}
[[ "$product_key" =~ ^[A-Z0-9]{5}(-[A-Z0-9]{5}){4}$ ]] || {
  echo "invalid Windows product key format" >&2
  exit 1
}
if virsh --connect qemu:///system dominfo "$name" >/dev/null 2>&1; then
  echo "VM already exists: $name" >&2
  exit 1
fi

xml_escape() {
  local value="$1"
  value="${value//&/&amp;}"; value="${value//</&lt;}"; value="${value//>/&gt;}"
  value="${value//\"/&quot;}"; value="${value//\'/&apos;}"
  printf '%s' "$value"
}

answer="$lab/generated/Autounattend.xml"
sed \
  -e "s|@@USER@@|$(xml_escape "$user")|g" \
  -e "s|@@PASSWORD@@|$(xml_escape "$NORTE_WINDOWS_PASSWORD")|g" \
  -e "s|@@IMAGE_NAME@@|$(xml_escape "$image_name")|g" \
  -e "s|@@LOCALE@@|$(xml_escape "$locale")|g" \
  -e "s|@@PRODUCT_KEY@@|$(xml_escape "$product_key")|g" \
  "$HERE/Autounattend.xml.in" >"$answer"
chmod 600 "$answer"

answer_iso="$lab/generated/unattend.iso"
xorriso -as mkisofs -quiet -J -r -o "$answer_iso" "$answer"
disk="$lab/disks/$name.qcow2"
[ ! -e "$disk" ] || {
  echo "disk already exists and will not be overwritten: $disk" >&2
  exit 1
}
qemu-img create -f qcow2 "$disk" "${disk_gib}G"

# qemu:///system runs the guest as a dedicated account. Grant only directory
# traversal and access to these exact VM files; this does not make $HOME
# listable by that account.
grant_qemu_traversal() {
  local parent owner
  parent="$(dirname "$(realpath -m "$1")")"
  while [ "$parent" != / ]; do
    owner="$(stat -c %u "$parent")"
    if [ "$owner" = "$(id -u)" ]; then
      setfacl -m "u:$qemu_user:x" "$parent"
    fi
    parent="$(dirname "$parent")"
  done
}
grant_qemu_traversal "$NORTE_WINDOWS_ISO"
grant_qemu_traversal "$disk"
setfacl -m "u:$qemu_user:r" "$NORTE_WINDOWS_ISO" "$answer_iso"
setfacl -m "u:$qemu_user:rw" "$disk"

virt-install --connect qemu:///system \
  --name "$name" --memory "$memory" --vcpus "$vcpus" --cpu host-passthrough \
  --machine q35 --boot uefi \
  --tpm backend.type=emulator,backend.version=2.0,model=tpm-crb \
  --disk "path=$disk,format=qcow2,bus=sata" \
  --cdrom "$NORTE_WINDOWS_ISO" \
  --disk "path=$answer_iso,device=cdrom,bus=sata" \
  --network network=default,model=e1000e \
  --graphics spice --video qxl --sound ich9 \
  --os-variant win11 --noautoconsole

# Microsoft installation media intentionally asks for a keypress before booting.
# Firmware startup time varies, so cover the short prompt window. These presses
# finish before Windows Setup loads.
for _ in 1 2 3 4; do
  sleep 2
  virsh --connect qemu:///system send-key "$name" KEY_SPACE
done

echo "created $name; open it with: virt-manager --connect qemu:///system"
echo "generated answer media contains a password: $answer_iso"
echo "temporary Windows password file: $password_file"
