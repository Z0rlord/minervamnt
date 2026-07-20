#!/bin/bash
# Reseed Wi-Fi + SSH + eth0 on an already-flashed Ubuntu Pi boot partition (no re-image).
# Usage:
#   WIFI_PASSWORD='literal-hotspot-password' \
#   EXTRA_WIFI_SSID='SST-WiFi' EXTRA_WIFI_PASSWORD='...' \
#   BOOT=/Volumes/system-boot \
#   PI_HOSTNAME=raspi-sd \
#   ROLE=primary \
#   bash scripts/reseed-pi-boot-wifi.sh
# Mount by disk ID (not /Volumes/system-boot name order) to avoid cross-seeding.set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

BOOT="${BOOT:?set BOOT=/Volumes/system-boot}"
HOSTNAME="${PI_HOSTNAME:-raspi-usb}"
ROLE="${ROLE:-media}" # primary|failover|media
WIFI_SSID="${WIFI_SSID:-Perseus}"
WIFI_PASSWORD="${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}"
# Optional second AP (e.g. SST-WiFi). Both are written into network-config access-points.
EXTRA_WIFI_SSID="${EXTRA_WIFI_SSID:-}"
EXTRA_WIFI_PASSWORD="${EXTRA_WIFI_PASSWORD:-}"
SSH_PUB="${SSH_PUB:-$(cat "$HOME/.ssh/raspi_key.pub")}"
INSTANCE_ID="pi5-${ROLE}-$(date +%Y%m%d%H%M%S)"
BOOT_ORDER_VALUE="${BOOT_ORDER_VALUE:-0xf41}"

log() { printf '[reseed] %s\n' "$*"; }
die() { printf '[reseed] ERROR: %s\n' "$*" >&2; exit 1; }

[[ -d "$BOOT" ]] || die "boot volume not found: $BOOT"

if grep -q 'os_prefix=current/' "$BOOT/config.txt" 2>/dev/null; then
  log "Ubuntu layout: boot uses current/cmdline.txt (os_prefix=current/)"
fi
if [[ -f "$BOOT/network-config" ]] && grep -qE 'password: "[0-9a-f]{64}"' "$BOOT/network-config"; then
  log "WARN: network-config has 64-char hex PSK (Imager) — replacing with literal password"
fi
if [[ -f "$BOOT/user-data" ]] && grep -q 'SHA256:' "$BOOT/user-data"; then
  log "WARN: user-data has SSH fingerprint not public key — replacing"
fi
if [[ -f "$BOOT/cmdline.txt" ]] && ! grep -q 'root=' "$BOOT/cmdline.txt"; then
  log "WARN: boot/cmdline.txt missing root= (Imager bug) — will only patch current/cmdline.txt"
fi
[[ -n "$WIFI_PASSWORD" ]] || die "set WIFI_PASSWORD to the literal Perseus hotspot password"
if [[ ${#WIFI_PASSWORD} -eq 64 && "$WIFI_PASSWORD" =~ ^[0-9a-fA-F]{64}$ && "${ALLOW_HEX_PSK:-}" != "1" ]]; then
  die "password looks like hex PSK — use the literal iPhone hotspot password"
fi
[[ -n "$SSH_PUB" ]] || die "missing raspi_key.pub"
if [[ -n "$EXTRA_WIFI_SSID" && -z "$EXTRA_WIFI_PASSWORD" ]]; then
  die "EXTRA_WIFI_SSID set but EXTRA_WIFI_PASSWORD is empty"
fi

WIFI_PASSWORD_YAML=$(printf '%s' "$WIFI_PASSWORD" | sed 's/\\/\\\\/g; s/"/\\"/g')
EXTRA_AP_YAML=""
EXTRA_SSID_LOG=""
if [[ -n "$EXTRA_WIFI_SSID" ]]; then
  EXTRA_WIFI_PASSWORD_YAML=$(printf '%s' "$EXTRA_WIFI_PASSWORD" | sed 's/\\/\\\\/g; s/"/\\"/g')
  EXTRA_AP_YAML=$(printf '        "%s":\n          password: "%s"\n' "$EXTRA_WIFI_SSID" "$EXTRA_WIFI_PASSWORD_YAML")
  EXTRA_SSID_LOG=" + ${EXTRA_WIFI_SSID}"
fi

cp "$BOOT/network-config" "$BOOT/network-config.bak-reseed" 2>/dev/null || true
cp "$BOOT/user-data" "$BOOT/user-data.bak-reseed" 2>/dev/null || true
cp "$BOOT/meta-data" "$BOOT/meta-data.bak-reseed" 2>/dev/null || true

cat >"$BOOT/meta-data" <<EOF
instance-id: ${INSTANCE_ID}
local-hostname: ${HOSTNAME}
dsmode: local
EOF

cat >"$BOOT/network-config" <<EOF
network:
  version: 2
  ethernets:
    eth0:
      dhcp4: true
      dhcp6: true
      optional: true
      addresses:
        - 192.168.2.2/24
  wifis:
    wlan0:
      dhcp4: true
      regulatory-domain: "JP"
      access-points:
        "${WIFI_SSID}":
          password: "${WIFI_PASSWORD_YAML}"
${EXTRA_AP_YAML}
      optional: false
EOF

cat >"$BOOT/set-boot-order.sh" <<EOF
#!/bin/bash
# Prefer SD card, then USB mass storage (Pi 5 EEPROM).
set -euo pipefail
LOG=/var/log/set-boot-order.log
exec >>"\$LOG" 2>&1
echo "=== \$(date -Is) set-boot-order role=${ROLE} ==="
command -v rpi-eeprom-config >/dev/null 2>&1 || { echo "no rpi-eeprom-config"; exit 0; }
TMP=\$(mktemp)
rpi-eeprom-config >"\$TMP" || exit 0
if grep -q '^BOOT_ORDER=' "\$TMP"; then
  sed -i "s/^BOOT_ORDER=.*/BOOT_ORDER=${BOOT_ORDER_VALUE}/" "\$TMP"
else
  echo "BOOT_ORDER=${BOOT_ORDER_VALUE}" >>"\$TMP"
fi
rpi-eeprom-config --apply "\$TMP" && echo "applied BOOT_ORDER=${BOOT_ORDER_VALUE}" || echo "apply failed"
rm -f "\$TMP"
EOF
chmod +x "$BOOT/set-boot-order.sh"

cat >"$BOOT/user-data" <<EOF
#cloud-config
hostname: ${HOSTNAME}
manage_etc_hosts: true
packages:
  - openssh-server
  - avahi-daemon
  - curl
  - ca-certificates
timezone: Asia/Tokyo
keyboard:
  model: pc105
  layout: "us"
users:
  - default
  - name: z0rlord
    gecos: z0rlord
    shell: /bin/bash
    lock_passwd: false
    groups: [sudo, adm]
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - ${SSH_PUB}
  - name: ubuntu
    shell: /bin/bash
    groups: [sudo, adm]
    sudo: ALL=(ALL) NOPASSWD:ALL
    ssh_authorized_keys:
      - ${SSH_PUB}
ssh_pwauth: true
ssh_deletekeys: false
write_files:
  - path: /etc/motd
    content: |
      Minerva Pi (${ROLE}) — ${HOSTNAME}
      Wi-Fi: ${WIFI_SSID}${EXTRA_SSID_LOG} | SSH: z0rlord / ubuntu + raspi_key
      Boot order goal: SD then USB (BOOT_ORDER=${BOOT_ORDER_VALUE})
      Ethernet: DHCP + 192.168.2.2/24 for direct Mac USB-LAN
  - path: /etc/systemd/system/set-boot-order.service
    permissions: "0644"
    content: |
      [Unit]
      Description=Set Pi EEPROM boot order SD then USB
      After=network-online.target
      ConditionPathExists=/boot/firmware/set-boot-order.sh
      [Service]
      Type=oneshot
      ExecStart=/bin/bash /boot/firmware/set-boot-order.sh
      RemainAfterExit=yes
      [Install]
      WantedBy=multi-user.target
runcmd:
  - [ bash, -lc, "chmod +x /boot/firmware/set-boot-order.sh 2>/dev/null || true" ]
  - [ systemctl, enable, --now, ssh ]
  - [ systemctl, enable, --now, avahi-daemon ]
  - [ systemctl, daemon-reload ]
  - [ systemctl, enable, --now, set-boot-order.service ]
EOF

# cloud-init nocloud must land on the cmdline the firmware actually boots
CMDLINE_FILES=()
if [[ -f "$BOOT/current/cmdline.txt" ]]; then
  CMDLINE_FILES+=("$BOOT/current/cmdline.txt")
elif [[ -f "$BOOT/cmdline.txt" ]] && grep -q 'root=' "$BOOT/cmdline.txt"; then
  CMDLINE_FILES+=("$BOOT/cmdline.txt")
fi
[[ ${#CMDLINE_FILES[@]} -gt 0 ]] || die "no bootable cmdline.txt with root= found under $BOOT"

for cmfile in "${CMDLINE_FILES[@]}"; do
  CM=$(tr -d '\n' <"$cmfile")
  CM=$(printf '%s' "$CM" | sed -E 's/ *ds=nocloud[^ ]*//g; s/ *cfg80211\.ieee80211_regdom=[^ ]*//g')
  printf '%s cfg80211.ieee80211_regdom=JP ds=nocloud;i=%s\n' "$CM" "$INSTANCE_ID" >"$cmfile"
  log "updated $cmfile"
done

sync
log "Reseeded $BOOT hostname=$HOSTNAME role=$ROLE ssid=$WIFI_SSID${EXTRA_SSID_LOG} password_len=${#WIFI_PASSWORD}"
log "Force cloud-init re-run via new instance-id=$INSTANCE_ID"
# Verify AP names without printing passwords
python3 - <<PY
from pathlib import Path
import re
nc = Path("$BOOT/network-config").read_text()
aps = re.findall(r'^\s+"([^"]+)":\s*$', nc, re.M)
print("[reseed] access-points:", ", ".join(aps))
PY
grep -E 'hostname:|ssh-ed25519|Perseus|SST-WiFi|dsmode|Minerva Pi' "$BOOT/user-data" "$BOOT/network-config" "$BOOT/meta-data" | grep -v password || true
