#!/usr/bin/env bash
# Watch for Pi SD mount OR USB ethernet link; patch boot or connect SSH.
set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BOOT="/Volumes/system-boot"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/raspi_key}"

log() { printf '[hub-watch] %s\n' "$*"; }

log "Watching for Pi SD (system-boot) or USB ethernet link (en6 active)..."
log "Cabling: Pi built-in RJ45 <--ethernet--> hub LAN port <---> Mac USB 10/100 LAN"
log "Press Ctrl+C to stop"

for i in $(seq 1 120); do
  if [[ -d "$BOOT" ]]; then
    log "SD boot volume detected — patching..."
    bash "$REPO/deploy/pi/patch-boot-ethernet-access.sh"
    log "Eject SD safely: diskutil unmountDisk /dev/disk4 2>/dev/null || diskutil unmount $BOOT"
    log "Insert SD in Pi and power on, then re-run this script for SSH"
    exit 0
  fi

  if ifconfig en6 2>/dev/null | grep -q 'status: active'; then
    log "USB ethernet link UP"
    bash "$REPO/deploy/pi/connect-ethernet-mac.sh" && exit 0
    for ip in 192.168.2.2 192.168.2.10; do
      if ssh -i "$SSH_KEY" -o BatchMode=yes -o ConnectTimeout=4 -o StrictHostKeyChecking=no \
        "ubuntu@$ip" 'hostname; ip -br a'; then
        log "Connected: ubuntu@$ip"
        exit 0
      fi
    done
  fi

  if (( i % 10 == 0 )); then
    st=$(ifconfig en6 2>/dev/null | awk '/status:/{print $2}')
    log "still waiting... en6=$st (try $i/120)"
  fi
  sleep 3
done

log "Timeout. Check:"
log "  - Pi powered ON (green ACT LED blinking)"
log "  - Ethernet cable: Pi RJ45 to hub, hub to Mac USB adapter"
log "  - Or power OFF Pi, put SD in hub reader so Mac mounts system-boot"
exit 1
