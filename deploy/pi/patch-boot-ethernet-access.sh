#!/usr/bin/env bash
# Patch Pi boot for USB-hub / direct-ethernet Mac setup + temporary LAN SSH.
set -euo pipefail

BOOT="${BOOT_VOL:-/Volumes/system-boot}"
RECOVERY_ID="pi5-hub-$(date +%Y%m%d%H%M)"
WIFI_SSID="${WIFI_SSID:-SST-WiFi}"
WIFI_PASS="${WIFI_PASS:-3445SST2805}"
SSH_PUB="${SSH_PUB:-$(cat "$HOME/.ssh/raspi_key.pub" 2>/dev/null || true)}"

log() { printf '[patch-boot] %s\n' "$*"; }

[[ -d "$BOOT" ]] || { echo "Mount Pi boot volume at $BOOT first (power OFF Pi, SD in hub reader)" >&2; exit 1; }
[[ -n "$SSH_PUB" ]] || { echo "Missing SSH public key" >&2; exit 1; }

BASE_CMDLINE=""
if [[ -f "$BOOT/current/cmdline.txt" ]]; then
  BASE_CMDLINE=$(tr -d '\n' <"$BOOT/current/cmdline.txt")
else
  BASE_CMDLINE="console=serial0,115200 multipath=off dwc_otg.lpm_enable=0 console=tty1 root=LABEL=writable rootfstype=ext4 panic=10 rootwait fixrtc"
fi

cp "$BOOT/user-data" "$BOOT/user-data.bak-hub-$(date +%Y%m%d%H%M)" 2>/dev/null || true
cp "$BOOT/network-config" "$BOOT/network-config.bak-hub-$(date +%Y%m%d%H%M)" 2>/dev/null || true

printf '%s\n' "${BASE_CMDLINE} cfg80211.ieee80211_regdom=US ds=nocloud;i=${RECOVERY_ID}" >"$BOOT/cmdline.txt"
printf 'instance-id: %s\n' "$RECOVERY_ID" >"$BOOT/meta-data"

cat >"$BOOT/network-config" <<EOF
#cloud-config
version: 2
ethernets:
  eth0:
    optional: true
    dhcp4: true
    addresses:
      - 192.168.2.2/24
wifis:
  wlan0:
    optional: false
    dhcp4: true
    regulatory-domain: US
    access-points:
      "${WIFI_SSID}":
        password: "${WIFI_PASS}"
EOF

cat >"$BOOT/user-data" <<EOF
#cloud-config
hostname: pi5
ssh_pwauth: true
users:
  - default
  - name: ubuntu
    ssh_authorized_keys:
      - ${SSH_PUB}
packages:
  - openssh-server
  - curl
  - ca-certificates
write_files:
  - path: /etc/systemd/system/pi5-lan-ssh-bootstrap.service
    permissions: "0644"
    content: |
      [Unit]
      Description=Allow SSH on eth0/wlan0 for hub bootstrap
      After=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/bin/bash -c 'ufw allow in on eth0 to any port 22 proto tcp || true; ufw allow in on wlan0 to any port 22 proto tcp || true; ufw reload || true'
      RemainAfterExit=yes
      [Install]
      WantedBy=multi-user.target
  - path: /etc/systemd/system/join-tailscale.service
    permissions: "0644"
    content: |
      [Unit]
      Description=Join Tailscale from boot partition script
      After=network-online.target
      Wants=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/bin/bash /boot/firmware/install-tailscale.sh
      RemainAfterExit=yes
      [Install]
      WantedBy=multi-user.target
bootcmd:
  - [ bash, -lc, "chmod +x /boot/firmware/install-tailscale.sh 2>/dev/null || true" ]
runcmd:
  - [ systemctl, daemon-reload ]
  - [ systemctl, enable, --now, pi5-lan-ssh-bootstrap.service ]
  - [ systemctl, unmask, ssh ]
  - [ systemctl, enable, ssh ]
  - [ systemctl, start, ssh ]
  - [ systemctl, enable, join-tailscale.service ]
  - [ systemctl, start, join-tailscale.service ]
EOF

rm -f "$BOOT"/FSCK*.REC 2>/dev/null || true
sync
log "Boot patched ($RECOVERY_ID)"
log "1. Eject SD, insert in Pi, power on"
log "2. Cable: Pi RJ45 <-> hub Ethernet <-> Mac USB LAN adapter"
log "3. Mac: bash deploy/pi/connect-ethernet-mac.sh"
log "4. SSH: ssh -i ~/.ssh/raspi_key ubuntu@192.168.2.2"
