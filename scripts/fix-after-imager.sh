#!/bin/bash
# Patch Pi Imager seeds on attached Ubuntu boot volume(s). Imager breaks Ubuntu 24.04 Pi:
# wrong cmdline file, hex WPA PSK, SSH fingerprint instead of pubkey.
# Usage:
#   WIFI_PASSWORD='your-iphone-hotspot-password' bash scripts/fix-after-imager.sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

WIFI_PASSWORD="${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}"
[[ -n "$WIFI_PASSWORD" ]] || {
  echo "Set WIFI_PASSWORD to the literal iPhone Personal Hotspot password." >&2
  exit 1
}

fix_one() {
  local boot="$1" hostname="$2"
  BOOT="$boot" PI_HOSTNAME="$hostname" WIFI_PASSWORD="$WIFI_PASSWORD" \
    bash "$ROOT/scripts/reseed-pi-boot-wifi.sh"
}

for boot in /Volumes/system-boot "/Volumes/system-boot 1"; do
  [[ -d "$boot" ]] || continue
  if [[ "$boot" == *" 1" ]]; then
    fix_one "$boot" "raspi-usb"
  else
    fix_one "$boot" "raspi-sd"
  fi
done

echo "Done. Eject, boot Pi on Perseus, then: bash scripts/find-pi.sh"
