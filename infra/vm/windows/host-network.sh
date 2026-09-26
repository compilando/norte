#!/usr/bin/env bash
# Keep Docker's FORWARD policy from shadowing libvirt's own NAT rules.
set -euo pipefail

mode="${1:---check}"
bridge="${NORTE_LIBVIRT_BRIDGE:-virbr0}"
helper=/usr/local/sbin/norte-libvirt-docker-network
unit=/etc/systemd/system/norte-libvirt-docker-network.service
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

has_rule() {
  iptables -C DOCKER-USER "$@" >/dev/null 2>&1
}

apply_rules() {
  iptables -S DOCKER-USER >/dev/null 2>&1 || return 0
  has_rule -i "$bridge" -j ACCEPT || iptables -I DOCKER-USER 1 -i "$bridge" -j ACCEPT
  has_rule -o "$bridge" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT || \
    iptables -I DOCKER-USER 2 -o "$bridge" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT
}

if [ "$mode" = --check ]; then
  if ! iptables -S DOCKER-USER >/dev/null 2>&1; then
    echo "ok   Docker has no DOCKER-USER chain"
  elif has_rule -i "$bridge" -j ACCEPT && \
       has_rule -o "$bridge" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT; then
    echo "ok   Docker permits libvirt NAT on $bridge"
  else
    echo "FAIL Docker blocks or may block libvirt NAT on $bridge" >&2
    echo "     run: pkexec $0 --install" >&2
    exit 1
  fi
elif [ "$mode" = --apply ]; then
  [ "$(id -u)" -eq 0 ] || { echo '--apply requires root' >&2; exit 1; }
  apply_rules
elif [ "$mode" = --install ]; then
  if [ "$(id -u)" -ne 0 ]; then
    exec pkexec "$0" --install
  fi
  install -m 0755 "$0" "$helper"
  install -m 0644 "$here/norte-libvirt-docker-network.service" "$unit"
  apply_rules
  systemctl daemon-reload
  systemctl enable norte-libvirt-docker-network.service
  echo "installed persistent Docker/libvirt forwarding rules for $bridge"
else
  echo "usage: $0 [--check|--apply|--install]" >&2
  exit 2
fi
