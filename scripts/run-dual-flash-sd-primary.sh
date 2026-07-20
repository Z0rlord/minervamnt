#!/bin/bash
# Run in Terminal.app (needs Full Disk Access for raw disk writes).
# SD = primary (disk4), USB 64GB = failover (disk5).
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LOG="$ROOT/.flash-dual.log"
IMG_LOCAL="$ROOT/.cache/ubuntu-24.04.4-preinstalled-server-arm64+raspi.img.xz"

cd "$ROOT"
mkdir -p "$ROOT/.cache"

echo "=== Dual flash: SD primary / USB failover ==="
diskutil list external physical

# Prefer password already in env; else read from currently seeded boot (literal, not hex)
if [[ -z "${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}" ]]; then
  WIFI_PASSWORD=$(python3 - <<'PY'
import re
from pathlib import Path
for p in [Path('/Volumes/system-boot/network-config'), Path('/Volumes/system-boot 1/network-config')]:
    if not p.exists():
        continue
    text = p.read_text(errors='replace')
    m = re.search(r'password:\s*"([^"]*)"', text)
    if m:
        print(m.group(1), end='')
        raise SystemExit(0)
raise SystemExit(1)
PY
) || {
    echo "Set WIFI_PASSWORD to the literal Perseus hotspot password, then re-run." >&2
    exit 2
  }
  export WIFI_PASSWORD
fi
export WIFI_PASSWORD="${WIFI_PASSWORD:-$PERSEUS_PASSWORD}"

python3 - <<'PY'
import os, re, sys
p=os.environ.get('WIFI_PASSWORD','')
if not p:
    sys.exit('empty WIFI_PASSWORD')
if re.fullmatch(r'[0-9a-fA-F]{64}', p):
    sys.exit('refusing 64-char hex PSK — use literal iPhone hotspot password')
print(f'password_ok len={len(p)} is_hex=False')
PY

[[ -f "$IMG_LOCAL" ]] || {
  echo "Missing image $IMG_LOCAL" >&2
  exit 1
}

echo "You will be prompted for your Mac password (sudo) to write /dev/rdisk*."
sudo -v

{
  echo "=== DUAL FLASH SD-PRIMARY $(date '+%Y-%m-%dT%H:%M:%S%z') ==="
  echo "Running as $(id) via Terminal"
  diskutil list external physical
} | tee -a "$LOG"

USB_DISK=disk5 SD_DISK=disk4 IMG_DIR="$ROOT/.cache" \
  WIFI_PASSWORD="$WIFI_PASSWORD" \
  bash "$ROOT/scripts/flash-dual-boot-perseus.sh" 2>&1 | tee -a "$LOG"
ec=${PIPESTATUS[0]}
echo "DUAL_FLASH_EXIT:$ec" | tee -a "$LOG"

if [[ $ec -eq 0 ]]; then
  echo
  echo "Verify password is not hex on both boots:"
  python3 - <<'PY'
import re
from pathlib import Path
for label, disk in [('disk4','/dev/disk4s1'), ('disk5','/dev/disk5s1')]:
    # find mount of that partition
    import subprocess
    out=subprocess.check_output(['mount'], text=True)
    boot=None
    for line in out.splitlines():
        if line.startswith(disk+' '):
            # path may contain spaces before (
            rest=line.split(' on ',1)[1]
            boot=rest.rsplit(' (',1)[0]
            break
    if not boot:
        print(label, 'not mounted')
        continue
    p=Path(boot)/'network-config'
    text=p.read_text(errors='replace')
    m=re.search(r'password:\s*"([^"]*)"', text)
    pw=m.group(1) if m else ''
    print(f"{label} boot={boot} hostname_grep=", end='')
    ud=(Path(boot)/'user-data').read_text(errors='replace')
    hm=re.search(r'^hostname:\s*(\S+)', ud, re.M)
    print(hm.group(1) if hm else '?', end=' ')
    print(f'pw_len={len(pw)} is_hex={bool(re.fullmatch(r"[0-9a-fA-F]{64}", pw))} eth_static={"192.168.2.2" in text}')
PY
fi
exit "$ec"
