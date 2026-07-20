#!/usr/bin/env bash
# Configure Mac USB Ethernet for direct Pi 5 link and discover SSH.
# Usage: bash deploy/pi/connect-ethernet-mac.sh
set -euo pipefail

PI_IP="${PI_IP:-192.168.2.2}"
MAC_IP="${MAC_IP:-192.168.2.1}"
NETMASK="${NETMASK:-255.255.255.0}"
USB_PORT="${USB_PORT:-USB 10/100 LAN}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/raspi_key}"
SSH_USER="${SSH_USER:-ubuntu}"

log() { printf '[ethernet] %s\n' "$*"; }

[[ "$(uname)" == "Darwin" ]] || { echo "macOS only" >&2; exit 1; }

log "Setting $USB_PORT -> $MAC_IP (static; Pi should be 192.168.2.2)"
networksetup -setmanual "$USB_PORT" "$MAC_IP" "$NETMASK" || true

log "Cabling check:"
log "  Pi built-in Ethernet (RJ45) <--cat5--> hub LAN port <--cat5--> this Mac USB adapter (en6)"
log "  USB-C hub on Mac only provides power/USB — you still need an Ethernet cable for en6"

# Wait for link (Pi powered on + cable to USB adapter)
for i in $(seq 1 30); do
  if ifconfig en6 2>/dev/null | grep -q 'status: active'; then
    log "Link up on en6"
    break
  fi
  log "Waiting for USB Ethernet link ($i/30) — power on Pi, cable to USB adapter"
  sleep 2
done

if ! ifconfig en6 2>/dev/null | grep -q 'status: active'; then
  log "WARN: en6 still inactive. Check: Pi power, cable, USB adapter LED."
fi

log "Pinging Pi at $PI_IP ..."
if ping -c2 -t3 "$PI_IP" >/dev/null 2>&1; then
  log "Pi responded to ping"
else
  log "No ping yet — if Pi is on router LAN instead, try: ssh -i $SSH_KEY $SSH_USER@100.75.188.125"
fi

for ip in "$PI_IP" 192.168.2.10 172.16.2.14; do
  if ssh -i "$SSH_KEY" -o BatchMode=yes -o ConnectTimeout=4 -o StrictHostKeyChecking=no \
    "$SSH_USER@$ip" 'hostname; ip -br a' 2>/dev/null; then
    log "SSH OK: $SSH_USER@$ip"
    echo "Run stack setup: ssh -i $SSH_KEY $SSH_USER@$ip 'bash -s' < deploy/pi/setup-node-stack.sh"
    exit 0
  fi
done

log "SSH not reachable yet."
log "If Pi SD is in the reader, run: bash deploy/pi/patch-boot-ethernet-access.sh"
log "Or wait for WiFi + Tailscale: ssh -i $SSH_KEY $SSH_USER@100.75.188.125"
exit 1
