#!/bin/bash
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
LOG="/Users/perseus-air/Projects/minervamnt/.flash-dual.log"
IMG_SRC="$HOME/Downloads/ubuntu-24.04.4-preinstalled-server-arm64+raspi.img.xz"
IMG_LOCAL="/Users/perseus-air/Projects/minervamnt/.cache/ubuntu-24.04.4-preinstalled-server-arm64+raspi.img.xz"
mkdir -p "$(dirname "$IMG_LOCAL")"
{
  echo "=== DUAL FLASH RETRY $(date '+%Y-%m-%dT%H:%M:%S%z') ==="
  echo "Running as $(id) from $0"
  diskutil list external physical
} | tee -a "$LOG"

# Prefer local copy outside Downloads (TCC)
if [[ ! -f "$IMG_LOCAL" ]]; then
  echo "[runner] Copying image out of Downloads to evade TCC..." | tee -a "$LOG"
  cp -f "$IMG_SRC" "$IMG_LOCAL"
fi
ls -lh "$IMG_LOCAL" | tee -a "$LOG"

export USB_DISK="${USB_DISK:-disk5}"
export SD_DISK="${SD_DISK:-disk4}"
export IMG_DIR="$(dirname "$IMG_LOCAL")"
# flash script uses IMG_DIR + fixed name — ensure filename matches
ln -sfn "$IMG_LOCAL" "$IMG_DIR/ubuntu-24.04.4-preinstalled-server-arm64+raspi.img.xz" 2>/dev/null || true

# Password must be literal hotspot password (not hex). Prefer env already set.
if [[ -z "${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}" ]]; then
  echo "ERROR: set WIFI_PASSWORD or PERSEUS_PASSWORD before running" | tee -a "$LOG"
  exit 2
fi

USB_DISK="$USB_DISK" SD_DISK="$SD_DISK" IMG_DIR="$IMG_DIR" \
  WIFI_PASSWORD="${WIFI_PASSWORD:-$PERSEUS_PASSWORD}" \
  /bin/bash /Users/perseus-air/Projects/minervamnt/scripts/flash-dual-boot-perseus.sh 2>&1 | tee -a "$LOG"
echo "DUAL_FLASH_EXIT:${PIPESTATUS[0]}" | tee -a "$LOG"
