#!/bin/bash
# Flash Ubuntu Server 24.04.4 to a Pi removable disk and seed Perseus Wi-Fi + SSH.
# Usage: DISK=disk5 PI_HOSTNAME=raspi-usb ROLE=primary ./scripts/flash-ubuntu-perseus.sh
set -euo pipefail
export PATH="/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin:$PATH"

DISK="${DISK:?set DISK=diskN}"
IMG_DIR="${IMG_DIR:-$HOME/Downloads}"
IMG_NAME="ubuntu-24.04.4-preinstalled-server-arm64+raspi.img.xz"
IMG_URL="https://cdimage.ubuntu.com/releases/24.04/release/${IMG_NAME}"
IMG_PATH="${IMG_DIR}/${IMG_NAME}"
HOSTNAME="${PI_HOSTNAME:-raspi-z0rlord-2}"
ROLE="${ROLE:-media}" # primary|failover|media — only affects MOTD / announce text
SSH_PUB="${SSH_PUB:-$(cat "$HOME/.ssh/raspi_key.pub")}"
# iPhone Personal Hotspot needs the LITERAL Wi-Fi password (Settings → Personal Hotspot).
# Do NOT use a 64-char hex PSK here unless you generated it with wpa_passphrase for this SSID.
WIFI_SSID="${WIFI_SSID:-Perseus}"
WIFI_PASSWORD="${WIFI_PASSWORD:-${PERSEUS_PASSWORD:-}}"
INSTANCE_ID="pi5-${ROLE}-$(date +%Y%m%d%H%M%S)"
# Default: SD (1) then USB (4) then restart (f) — nibble order is right-to-left
BOOT_ORDER_VALUE="${BOOT_ORDER_VALUE:-0xf41}"

log() { printf '[flash-pi] %s\n' "$*"; }
die() { printf '[flash-pi] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(uname)" == "Darwin" ]] || die "macOS only"
[[ -n "$SSH_PUB" ]] || die "missing SSH public key"
[[ "$DISK" != "disk0" && "$DISK" != "disk1" ]] || die "refusing internal disk $DISK"
[[ -n "$WIFI_PASSWORD" ]] || die "set WIFI_PASSWORD or PERSEUS_PASSWORD to the literal hotspot password (not a hex PSK)"
if [[ ${#WIFI_PASSWORD} -eq 64 && "$WIFI_PASSWORD" =~ ^[0-9a-fA-F]{64}$ && "${ALLOW_HEX_PSK:-}" != "1" ]]; then
  die "WIFI_PASSWORD looks like a 64-char hex PSK; iPhone hotspots need the literal password. Re-export the real password, or set ALLOW_HEX_PSK=1 if intentional."
fi

log "Verifying target $DISK (role=$ROLE hostname=$HOSTNAME) ..."
info=$(diskutil info "$DISK" 2>/dev/null) || die "diskutil info $DISK failed"
echo "$info" | grep -E 'Removable Media:.*Removable|Protocol:.*USB' >/dev/null \
  || die "$DISK does not look like removable USB"
SIZE=$(echo "$info" | awk -F'[()]' '/Disk Size/ {print $2}' | awk '{print $1}' | tr -d ',')
log "Target size bytes=$SIZE"
if [[ -z "$SIZE" || "$SIZE" -lt 15000000000 || "$SIZE" -gt 140000000000 ]]; then
  die "unexpected size $SIZE — aborting"
fi

if [[ ! -f "$IMG_PATH" ]]; then
  log "Downloading $IMG_URL ..."
  mkdir -p "$IMG_DIR"
  curl -L --fail --progress-bar -o "$IMG_PATH.partial" "$IMG_URL"
  mv "$IMG_PATH.partial" "$IMG_PATH"
else
  log "Using existing image $IMG_PATH"
fi

log "Unmounting $DISK ..."
diskutil unmountDisk force "/dev/$DISK" || true
sleep 2

RAW="/dev/r${DISK}"
[[ -e "$RAW" ]] || RAW="/dev/${DISK}"
log "Writing image to $RAW ..."
if [[ "$(id -u)" -eq 0 ]]; then
  xz -dc "$IMG_PATH" | dd of="$RAW" bs=4m status=progress
else
  xz -dc "$IMG_PATH" | sudo dd of="$RAW" bs=4m status=progress
fi
sync
log "Write complete."

sleep 3
diskutil mountDisk "/dev/$DISK" 2>/dev/null || diskutil mount "${DISK}s1" 2>/dev/null || true
sleep 3

# Resolve mount path for THIS disk only (paths may contain spaces, e.g. "system-boot 1")
boot_mount_for_disk() {
  mount | awk -v d="/dev/${1}s1" '
    $1 == d {
      sub(/^[^ ]+ on /, "");
      sub(/ \(.*$/, "");
      print;
      exit
    }'
}

BOOT=""
for _ in $(seq 1 40); do
  diskutil mountDisk "/dev/$DISK" 2>/dev/null || diskutil mount "${DISK}s1" 2>/dev/null || true
  mp=$(boot_mount_for_disk "$DISK")
  if [[ -n "$mp" && -d "$mp" ]] && [[ -f "$mp/config.txt" || -f "$mp/cmdline.txt" || -f "$mp/user-data" || -f "$mp/current/cmdline.txt" ]]; then
    BOOT="$mp"
    break
  fi
  sleep 2
done
[[ -d "$BOOT" ]] || die "boot partition for $DISK did not mount (got BOOT='${BOOT:-}')"
# Hard guarantee: never seed another disk's volume
mp=$(boot_mount_for_disk "$DISK")
[[ "$BOOT" == "$mp" ]] || die "refusing to seed $BOOT — belongs to other disk (expected $mp for $DISK)"

log "Seeding cloud-init on $BOOT ..."
cp "$BOOT/network-config" "$BOOT/network-config.stock" 2>/dev/null || true
cp "$BOOT/user-data" "$BOOT/user-data.stock" 2>/dev/null || true

cat >"$BOOT/meta-data" <<EOF
instance-id: ${INSTANCE_ID}
local-hostname: ${HOSTNAME}
dsmode: local
EOF

# Escape YAML double-quoted password
WIFI_PASSWORD_YAML=$(printf '%s' "$WIFI_PASSWORD" | sed 's/\\/\\\\/g; s/"/\\"/g')

cat >"$BOOT/network-config" <<EOF
network:
  version: 2
  ethernets:
    eth0:
      dhcp4: true
      dhcp6: true
      optional: true
      # Direct Mac USB-LAN link (Mac sets 192.168.2.1); also works with DHCP routers
      addresses:
        - 192.168.2.2/24
  wifis:
    wlan0:
      dhcp4: true
      regulatory-domain: "JP"
      access-points:
        "${WIFI_SSID}":
          password: "${WIFI_PASSWORD_YAML}"
      optional: false
EOF

cat >"$BOOT/set-boot-order.sh" <<EOF
#!/bin/bash
# Prefer SD card, then USB mass storage (Pi 5 EEPROM). Override via BOOT_ORDER_VALUE.
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
rpi-eeprom-config --apply "\$TMP" && echo "applied BOOT_ORDER=${BOOT_ORDER_VALUE}" || echo "apply failed (ok if not Pi firmware tools)"
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
      Wi-Fi: Perseus | SSH: z0rlord / ubuntu + raspi_key
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

# nocloud must be on the cmdline the Pi firmware boots (current/ when os_prefix=current/)
CMDLINE_FILES=()
if [[ -f "$BOOT/current/cmdline.txt" ]]; then
  CMDLINE_FILES+=("$BOOT/current/cmdline.txt")
elif [[ -f "$BOOT/cmdline.txt" ]] && grep -q 'root=' "$BOOT/cmdline.txt"; then
  CMDLINE_FILES+=("$BOOT/cmdline.txt")
fi
[[ ${#CMDLINE_FILES[@]} -gt 0 ]] || die "no bootable cmdline.txt with root= under $BOOT"
for cmfile in "${CMDLINE_FILES[@]}"; do
  CM=$(tr -d '\n' <"$cmfile")
  CM=$(printf '%s' "$CM" | sed -E 's/ *ds=nocloud[^ ]*//g; s/ *cfg80211\.ieee80211_regdom=[^ ]*//g')
  printf '%s cfg80211.ieee80211_regdom=JP ds=nocloud;i=%s\n' "$CM" "$INSTANCE_ID" >"$cmfile"
  log "updated $cmfile"
done

sync
log "Seeded $BOOT"
grep -E 'hostname:|ssh-ed25519|Perseus' "$BOOT/user-data" "$BOOT/network-config" | grep -v password || true
printf 'FLASH_OK disk=%s hostname=%s role=%s boot=%s\n' "$DISK" "$HOSTNAME" "$ROLE" "$BOOT"
