#!/usr/bin/env bash
# Patch Pi boot partition to:
# - keep eth0 on DHCP (works on hotel/router networks)
# - ensure WiFi config exists (optional; SST-WiFi default)
# - temporarily allow LAN SSH on eth0/wlan0 (so we can reach the Pi)
#
# Run this from macOS when the Pi SD boot partition is mounted at:
#   /Volumes/system-boot
#
# Safe-ish: preserves the kernel cmdline except we append cloud-init + regdom.
set -euo pipefail

BOOT="${BOOT_VOL:-/Volumes/system-boot}"
RECOVERY_ID="pi5-lan-ssh-$(date +%Y%m%d%H%M)"

WIFI_SSID="${WIFI_SSID:-SST-WiFi}"
WIFI_PASS="${WIFI_PASS:-3445SST2805}"
WIFI_COUNTRY="${WIFI_COUNTRY:-US}"

SSH_PUB="${SSH_PUB:-$(cat "$HOME/.ssh/raspi_key.pub" 2>/dev/null || true)}"
[[ -n "$SSH_PUB" ]] || { echo "Missing SSH pubkey: $HOME/.ssh/raspi_key.pub" >&2; exit 1; }

log() { printf '[patch-lan-ssh] %s\n' "$*"; }

[[ -d "$BOOT" ]] || { echo "Mount Pi boot volume at $BOOT first" >&2; exit 1; }

BASE_CMDLINE=""
if [[ -f "$BOOT/current/cmdline.txt" ]]; then
  BASE_CMDLINE="$(tr -d '\n' <"$BOOT/current/cmdline.txt")"
else
  BASE_CMDLINE="console=serial0,115200 multipath=off dwc_otg.lpm_enable=0 console=tty1 root=LABEL=writable rootfstype=ext4 panic=10 rootwait fixrtc"
fi

# Cloud-init marker (do not replace cmdline wholesale; append instead).
# Note: ds=nocloud;i=... must be present so that cloud-init re-runs.
printf '%s\n' "${BASE_CMDLINE} cfg80211.ieee80211_regdom=${WIFI_COUNTRY} ds=nocloud;i=${RECOVERY_ID}" >"$BOOT/cmdline.txt"
printf 'instance-id: %s\n' "$RECOVERY_ID" >"$BOOT/meta-data"

cat >"$BOOT/network-config" <<EOF
#cloud-config
version: 2
ethernets:
  eth0:
    optional: true
    dhcp4: true
wifis:
  wlan0:
    optional: false
    dhcp4: true
    regulatory-domain: ${WIFI_COUNTRY}
    access-points:
      "${WIFI_SSID}":
        password: "${WIFI_PASS}"
EOF

# Overwrite user-data with a cloud-config that:
# - sets SSH key for ubuntu user
# - enables a temporary systemd service to open LAN SSH via ufw
# - starts tailscale using the existing /boot/firmware/install-tailscale.sh + tailscale-authkey
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
      Description=Temporary LAN SSH for bootstrap (pi5-lan-ssh-bootstrap)
      After=network-online.target
      Wants=network-online.target

      [Service]
      Type=oneshot
      ExecStart=/bin/bash -lc 'set -e; \
        ufw allow in on eth0 to any port 22 proto tcp 2>/dev/null || true; \
        ufw allow in on wlan0 to any port 22 proto tcp 2>/dev/null || true; \
        ufw reload 2>/dev/null || true; \
        systemctl unmask ssh sshd 2>/dev/null || true; \
        systemctl enable ssh 2>/dev/null || systemctl enable sshd 2>/dev/null || true; \
        systemctl restart ssh 2>/dev/null || systemctl restart sshd 2>/dev/null || true'
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

runcmd:
  - [ systemctl, daemon-reload ]
  - [ systemctl, enable, --now, pi5-lan-ssh-bootstrap.service ]
  - [ systemctl, enable, join-tailscale.service ]
  - [ systemctl, start, join-tailscale.service ]
EOF

rm -f "$BOOT"/FSCK*.REC 2>/dev/null || true
sync

log "Patched $BOOT (instance $RECOVERY_ID)"
log "Reinsert SD into Pi, power-cycle Pi, then we can locate it via LAN SSH scan."

