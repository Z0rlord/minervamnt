#!/bin/bash
# Finish failover USB flash after SD-primary dual flash (disk5 was cross-seeded / unplugged).
# Usage: plug in the ~64GB USB stick, then:
#   WIFI_PASSWORD='...' bash scripts/finish-usb-failover-flash.sh
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG="$ROOT/.flash-dual.log"
IMG_DIR="$ROOT/.cache"

[[ -n "${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}" ]] || {
  # Try SD boot volume
  if [[ -f /Volumes/system-boot/network-config ]]; then
    WIFI_PASSWORD=$(python3 - <<'PY'
import re
from pathlib import Path
print(re.search(r'password:\s*"([^"]*)"', Path('/Volumes/system-boot/network-config').read_text()).group(1), end='')
PY
)
    export WIFI_PASSWORD
  fi
}
export WIFI_PASSWORD="${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}"
[[ -n "$WIFI_PASSWORD" ]] || { echo "set WIFI_PASSWORD" >&2; exit 2; }

python3 - <<'PY'
import os, re, sys
p=os.environ['WIFI_PASSWORD']
if re.fullmatch(r'[0-9a-fA-F]{64}', p):
    sys.exit('refusing hex PSK')
print(f'password_ok len={len(p)}')
PY

# Find ~62GB external disk
USB_DISK=""
while read -r id; do
  [[ -n "$id" ]] || continue
  size=$(diskutil info "$id" | awk -F'[()]' '/Disk Size/ {print $2}' | awk '{print $1}' | tr -d ',')
  # ~50–70 GB
  if [[ -n "$size" && "$size" -gt 50000000000 && "$size" -lt 70000000000 ]]; then
    USB_DISK="$id"
    break
  fi
done < <(diskutil list external physical | awk '/^\// {print $1}' | sed 's|/dev/||')

[[ -n "$USB_DISK" ]] || { echo "No ~64GB USB disk found — plug it in and retry." >&2; diskutil list external physical; exit 1; }
echo "Using USB_DISK=$USB_DISK"

sudo -v
DISK="$USB_DISK" PI_HOSTNAME=raspi-usb ROLE=failover BOOT_ORDER_VALUE=0xf41 \
  IMG_DIR="$IMG_DIR" WIFI_PASSWORD="$WIFI_PASSWORD" \
  bash "$ROOT/scripts/flash-ubuntu-perseus.sh" 2>&1 | tee -a "$LOG"

# Verify mount belongs to USB_DISK
python3 - <<PY
import re, subprocess
from pathlib import Path
disk="$USB_DISK"
out=subprocess.check_output(['mount'], text=True)
boot=None
for line in out.splitlines():
    if line.startswith(f'/dev/{disk}s1 '):
        boot=line.split(' on ',1)[1].rsplit(' (',1)[0]
        break
assert boot, f'{disk}s1 not mounted'
ud=(Path(boot)/'user-data').read_text(); nc=(Path(boot)/'network-config').read_text()
hm=re.search(r'^hostname:\s*(\S+)', ud, re.M).group(1)
pw=re.search(r'password:\s*"([^"]*)"', nc).group(1)
assert hm=='raspi-usb', hm
assert 'failover' in ud
assert '192.168.2.2/24' in nc
assert not re.fullmatch(r'[0-9a-fA-F]{64}', pw)
print(f'USB_OK boot={boot} hostname={hm} pw_len={len(pw)} eth=True')
PY
