#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
[ ! -f "$HERE/config.env" ] || source "$HERE/config.env"
virsh --connect qemu:///system shutdown "${NORTE_WINDOWS_VM_NAME:-norte-win11-build}"
