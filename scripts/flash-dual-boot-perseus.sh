#!/bin/bash
# Flash SD (primary) then USB thumb (failover) for Pi 5 dual-media boot.
# Requires: WIFI_PASSWORD or PERSEUS_PASSWORD = literal iPhone hotspot password
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FLASH="$ROOT/scripts/flash-ubuntu-perseus.sh"
USB_DISK="${USB_DISK:-disk5}"
SD_DISK="${SD_DISK:-disk4}"
# SD (1) then USB (4) then restart (f) — nibble order is right-to-left
BOOT_ORDER_VALUE="${BOOT_ORDER_VALUE:-0xf41}"

[[ -n "${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}" ]] || {
  echo "Set WIFI_PASSWORD to the literal Perseus hotspot password (iPhone Settings → Personal Hotspot)." >&2
  echo "Do not use a 64-char hex PSK." >&2
  exit 1
}

log() { printf '[dual-flash] %s\n' "$*"; }

log "=== 1/2 SD primary ($SD_DISK -> raspi-sd) ==="
DISK="$SD_DISK" PI_HOSTNAME=raspi-sd ROLE=primary BOOT_ORDER_VALUE="$BOOT_ORDER_VALUE" bash "$FLASH"

log "=== 2/2 USB failover ($USB_DISK -> raspi-usb) ==="
DISK="$USB_DISK" PI_HOSTNAME=raspi-usb ROLE=failover BOOT_ORDER_VALUE="$BOOT_ORDER_VALUE" bash "$FLASH"

cat <<EOF

=== DUAL FLASH COMPLETE ===
Primary (SD card):    $SD_DISK  hostname=raspi-sd
Failover (USB thumb): $USB_DISK  hostname=raspi-usb
Both: Ubuntu 24.04.4 Server + Perseus Wi-Fi + eth0 DHCP/static + SSH (z0rlord/ubuntu + raspi_key)
Boot order (set on first successful boot): SD then USB (BOOT_ORDER=${BOOT_ORDER_VALUE})

Eject both:
  diskutil eject /dev/${SD_DISK}
  diskutil eject /dev/${USB_DISK}

In the Pi:
  1. Insert SD into Pi slot (primary)
  2. Plug USB thumb into a USB-A/USB-C port on the Pi (powered hub OK)
  3. Power on — prefers SD once EEPROM is updated
  4. First boot may take 3–5 min

SSH:
  ssh -i ~/.ssh/raspi_key z0rlord@raspi-sd.local
  ssh -i ~/.ssh/raspi_key z0rlord@raspi-usb.local
  # direct Ethernet to Mac USB LAN (Mac=192.168.2.1):
  ssh -i ~/.ssh/raspi_key z0rlord@192.168.2.2

Confirm boot order after first boot:
  sudo rpi-eeprom-config | grep BOOT_ORDER

EOF
