#!/usr/bin/env bash
# Patch Pi 5 boot partition (system-boot) for Tailscale + ZeroTier with interactive auth.
# Run on macOS while SD is mounted (e.g. disk4 -> /Volumes/system-boot).
#
#   bash deploy/pi/patch-boot-vpn.sh
#   BOOT_VOL=/Volumes/system-boot DISK=disk4 bash deploy/pi/patch-boot-vpn.sh
#
# Tailscale: no auth key on SD — first boot prints a login URL (approve in browser).
# ZeroTier: optional network ID file on boot partition; approve node at my.zerotier.com.
set -euo pipefail

BOOT="${BOOT_VOL:-/Volumes/system-boot}"
DISK="${DISK:-}"
RECOVERY_ID="pi5-vpn-$(date +%Y%m%d%H%M)"
HOSTNAME="${PI_HOSTNAME:-raspi-z0rlord}"
SSH_PUB="${SSH_PUB:-$(cat "$HOME/.ssh/raspi_key.pub" 2>/dev/null || cat "$HOME/.ssh/id_ed25519.pub" 2>/dev/null || true)}"
ZT_NETWORK_ID="${ZT_NETWORK_ID:-}"

log() { printf '[patch-vpn] %s\n' "$*"; }
die() { printf '[patch-vpn] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(uname)" == "Darwin" ]] || die "macOS only"
[[ -d "$BOOT" ]] || die "Mount Pi boot volume at $BOOT first (power OFF Pi, SD in reader)"

if [[ -z "$DISK" ]]; then
  DISK=$(mount | awk '$3 == "'"$BOOT"'" {print $1}' | sed -E 's|.*/(disk[0-9]+).*$|\1|')
fi
[[ -n "$DISK" ]] || die "Could not detect disk id — set DISK=disk4"

log "Boot volume: $BOOT (disk: $DISK)"

if [[ -z "$ZT_NETWORK_ID" ]]; then
  read -r -p "ZeroTier network ID (16 chars, blank to skip ZeroTier join): " ZT_NETWORK_ID
fi

# --- install scripts on boot partition (copied to /boot/firmware on Pi) ---
tee "$BOOT/install-tailscale.sh" >/dev/null <<'INSTALL_TS'
#!/usr/bin/env bash
# Join Tailscale — interactive browser auth (no auth key on SD).
set -euo pipefail

MARKER=/var/lib/tailscale-joined
LOG=/var/log/join-tailscale.log
PROMPT=/boot/firmware/tailscale-auth-url.txt
HOST="${TS_HOSTNAME:-$(hostname -s)}"

exec >>"$LOG" 2>&1
echo "=== join-tailscale $(date -Is) ==="

if [[ -f "$MARKER" ]] && tailscale status >/dev/null 2>&1; then
  echo "Already joined."
  exit 0
fi

if ! command -v tailscale >/dev/null 2>&1; then
  curl -fsSL https://tailscale.com/install.sh | sh
fi

systemctl enable --now tailscaled
sleep 2

if tailscale status --json 2>/dev/null | grep -q '"BackendState":"Running"'; then
  touch "$MARKER"
  exit 0
fi

AUTH_URL=""
if AUTH_URL=$(tailscale up --hostname="$HOST" --accept-routes --reset --print-auth-url 2>/dev/null); then
  :
else
  AUTH_URL=$(tailscale up --hostname="$HOST" --accept-routes --print-auth-url 2>&1 | grep -Eo 'https://login\.tailscale\.com/[^[:space:]]+' | head -1 || true)
fi

MSG=$(
  cat <<EOF

========================================
TAILSCALE — authorize this Pi in a browser:
${AUTH_URL:-<run: sudo tailscale up --print-auth-url>}

  ssh: cat /boot/firmware/tailscale-auth-url.txt
========================================

EOF
)

printf '%s\n' "$AUTH_URL" | tee "$PROMPT" >/dev/null
printf '%s' "$MSG" | tee /dev/console >/dev/null
command -v wall >/dev/null && printf '%s' "$MSG" | wall || true

echo "Waiting up to 15 minutes for Tailscale authorization..."
if tailscale up --hostname="$HOST" --accept-routes --timeout=15m; then
  touch "$MARKER"
  rm -f "$PROMPT"
  echo "Tailscale joined: $(tailscale ip -4 2>/dev/null || true)"
else
  echo "Tailscale not authorized yet — re-run: sudo bash /boot/firmware/install-tailscale.sh"
  exit 1
fi
INSTALL_TS

tee "$BOOT/install-zerotier.sh" >/dev/null <<'INSTALL_ZT'
#!/usr/bin/env bash
# Install ZeroTier; join networks listed in /boot/firmware/zerotier-network-id (one ID per line).
set -euo pipefail

MARKER=/var/lib/zerotier-joined
LOG=/var/log/join-zerotier.log
NET_FILE=/boot/firmware/zerotier-network-id
NODE_FILE=/boot/firmware/zerotier-node-id.txt

exec >>"$LOG" 2>&1
echo "=== join-zerotier $(date -Is) ==="

if [[ ! -f "$NET_FILE" ]]; then
  echo "No $NET_FILE — skip ZeroTier join (create file on SD to enable)."
  exit 0
fi

mapfile -t NET_IDS < <(grep -v '^[[:space:]]*#' "$NET_FILE" | tr -d '[:space:]' | grep -E '^[0-9a-fA-F]{16}$' || true)
if ((${#NET_IDS[@]} == 0)); then
  echo "No valid network IDs in $NET_FILE"
  exit 0
fi

if ! command -v zerotier-cli >/dev/null 2>&1; then
  curl -sSf https://install.zerotier.com | bash
fi

systemctl enable --now zerotier-one
sleep 2

NODE_ID=$(zerotier-cli info 2>/dev/null | awk '{print $3}')
printf '%s\n' "$NODE_ID" >"$NODE_FILE"

for NET_ID in "${NET_IDS[@]}"; do
  if ! zerotier-cli listnetworks 2>/dev/null | grep -q "$NET_ID"; then
    zerotier-cli join "$NET_ID" || true
  fi
done

NET_LIST=$(printf '%s\n' "${NET_IDS[@]}")

MSG=$(
  cat <<EOF

========================================
ZEROTIER — approve this node in the dashboard:
  https://my.zerotier.com
  Node ID:    ${NODE_ID}

  Networks to approve:
${NET_LIST}

  ssh: cat /boot/firmware/zerotier-node-id.txt
========================================

EOF
)

printf '%s' "$MSG" | tee /dev/console >/dev/null
command -v wall >/dev/null && printf '%s' "$MSG" | wall || true

for _ in $(seq 1 60); do
  pending=0
  for NET_ID in "${NET_IDS[@]}"; do
    if ! zerotier-cli listnetworks 2>/dev/null | awk -v n="$NET_ID" '$1 ~ n && $4 == "OK" {found=1} END{exit !found}'; then
      pending=1
    fi
  done
  if (( pending == 0 )); then
    touch "$MARKER"
    echo "ZeroTier OK on all networks"
    zerotier-cli listnetworks
    exit 0
  fi
  sleep 10
done

echo "ZeroTier join pending — approve node ${NODE_ID} for: ${NET_IDS[*]}"
exit 1
INSTALL_ZT

chmod +x "$BOOT/install-tailscale.sh" "$BOOT/install-zerotier.sh"

if [[ -n "$ZT_NETWORK_ID" ]]; then
  printf '%s\n' "$ZT_NETWORK_ID" >"$BOOT/zerotier-network-id"
  log "Wrote ZeroTier network ID to boot partition"
else
  rm -f "$BOOT/zerotier-network-id" 2>/dev/null || true
  log "ZeroTier: install only (no network id file — add zerotier-network-id later)"
fi

# --- cloud-init: force re-run + VPN services ---
ts=$(date +%Y%m%d%H%M)
cp "$BOOT/user-data" "$BOOT/user-data.bak-vpn-$ts" 2>/dev/null || true
cp "$BOOT/cmdline.txt" "$BOOT/cmdline.txt.bak-vpn-$ts" 2>/dev/null || true
cp "$BOOT/meta-data" "$BOOT/meta-data.bak-vpn-$ts" 2>/dev/null || true

BASE_CMDLINE=""
if [[ -f "$BOOT/current/cmdline.txt" ]]; then
  BASE_CMDLINE=$(tr -d '\n' <"$BOOT/current/cmdline.txt")
elif [[ -f "$BOOT/cmdline.txt.bak-vpn-$ts" ]]; then
  BASE_CMDLINE=$(tr -d '\n' <"$BOOT/cmdline.txt.bak-vpn-$ts" | sed -E 's/(^| )ds=nocloud[^ ]*//g; s/(^| )cfg80211\.ieee80211_regdom=[^ ]*//g')
fi
[[ -n "$BASE_CMDLINE" ]] || BASE_CMDLINE="console=serial0,115200 multipath=off dwc_otg.lpm_enable=0 console=tty1 root=LABEL=writable rootfstype=ext4 panic=10 rootwait fixrtc"

printf '%s\n' "${BASE_CMDLINE} cfg80211.ieee80211_regdom=US ds=nocloud;i=${RECOVERY_ID}" >"$BOOT/cmdline.txt"
printf 'instance-id: %s\n' "$RECOVERY_ID" >"$BOOT/meta-data"

# Fix SSH key if user-data has fingerprint instead of pubkey
SSH_BLOCK=""
if [[ -n "$SSH_PUB" ]]; then
  SSH_BLOCK="  ssh_authorized_keys:
    - \"${SSH_PUB}\""
fi

tee "$BOOT/user-data" >/dev/null <<EOF
#cloud-config
manage_resolv_conf: false
hostname: ${HOSTNAME}
manage_etc_hosts: true
packages:
  - avahi-daemon
  - curl
  - ca-certificates
apt:
  preserve_sources_list: true
  conf: |
    Acquire {
      Check-Date "false";
    };
timezone: Asia/Tokyo
keyboard:
  model: pc105
  layout: "us"
user:
  name: z0rlord
  shell: /bin/bash
  lock_passwd: false
  passwd: "\$y\$jB5\$3FJSDRYAoM3GKBJJfZ3N1/\$TQdBAAGrp.tF96SEfo/Npn27ay9arLKjsRoOWkq.n63"
${SSH_BLOCK}
ssh_pwauth: false

write_files:
  - path: /etc/systemd/system/join-tailscale.service
    permissions: "0644"
    content: |
      [Unit]
      Description=Join Tailscale (interactive browser auth)
      After=network-online.target
      Wants=network-online.target
      [Service]
      Type=oneshot
      Environment=TS_HOSTNAME=${HOSTNAME}
      ExecStart=/bin/bash /boot/firmware/install-tailscale.sh
      RemainAfterExit=yes
      [Install]
      WantedBy=multi-user.target

  - path: /etc/systemd/system/join-zerotier.service
    permissions: "0644"
    content: |
      [Unit]
      Description=Join ZeroTier (approve at my.zerotier.com)
      After=network-online.target join-tailscale.service
      Wants=network-online.target
      [Service]
      Type=oneshot
      ExecStart=/bin/bash /boot/firmware/install-zerotier.sh
      RemainAfterExit=yes
      [Install]
      WantedBy=multi-user.target

bootcmd:
  - [ bash, -lc, "chmod +x /boot/firmware/install-tailscale.sh /boot/firmware/install-zerotier.sh 2>/dev/null || true" ]

runcmd:
  - [ systemctl, enable, --now, ssh ]
  - [ systemctl, daemon-reload ]
  - [ systemctl, enable, join-tailscale.service ]
  - [ systemctl, start, join-tailscale.service ]
  - [ systemctl, enable, join-zerotier.service ]
  - [ systemctl, start, join-zerotier.service ]
EOF

rm -f "$BOOT"/FSCK*.REC 2>/dev/null || true
sync

log "Patched $BOOT (instance-id: $RECOVERY_ID)"
log "Scripts: install-tailscale.sh, install-zerotier.sh"

cat <<EOF

=== NEXT: boot Pi and authorize ===
1. Eject SD safely:
     diskutil unmountDisk /dev/${DISK}

2. Insert SD in Pi 5, power on, wait ~2–3 min (Wi‑Fi: SST-WIFI).

3. TAILSCALE — open the login URL (pick one):
     • HDMI monitor on Pi (message on console)
     • From Mac once SSH works:
         ssh z0rlord@${HOSTNAME}.local 'cat /boot/firmware/tailscale-auth-url.txt'
     • Or re-print:
         ssh z0rlord@${HOSTNAME}.local 'sudo tailscale up --print-auth-url'

4. ZEROTIER (if you set a network ID) — approve node at https://my.zerotier.com
     ssh z0rlord@${HOSTNAME}.local 'cat /boot/firmware/zerotier-node-id.txt'

5. Verify:
     ssh z0rlord@${HOSTNAME}.local 'tailscale ip -4; zerotier-cli listnetworks'

EOF

read -r -p "Unmount SD card now? [Y/n] " ans
ans_lc=$(printf '%s' "$ans" | tr '[:upper:]' '[:lower:]')
if [[ "$ans_lc" != "n" ]]; then
  diskutil unmountDisk "/dev/${DISK}" 2>/dev/null || diskutil unmount "$BOOT" 2>/dev/null || true
  log "SD unmounted — insert in Pi and boot."
fi

read -r -p "Press Enter when Pi has booted ~3 min (then we'll try to fetch Tailscale auth URL)..." _

TS_URL=""
for host in "${HOSTNAME}.local" "raspi-z0rlord.local" "pi5.local"; do
  if TS_URL=$(ssh -o ConnectTimeout=5 -o StrictHostKeyChecking=no -o BatchMode=yes \
      "z0rlord@${host}" 'cat /boot/firmware/tailscale-auth-url.txt 2>/dev/null' 2>/dev/null); then
    [[ -n "$TS_URL" ]] && break
  fi
done

if [[ -n "$TS_URL" ]]; then
  log "TAILSCALE — authorize in browser:"
  printf '%s\n' "$TS_URL"
  read -r -p "Press Enter after you've authorized Tailscale in the browser..." _
else
  log "Could not SSH yet — authorize Tailscale manually when Pi is reachable."
fi

if [[ -f "$BOOT/zerotier-network-id" ]] || [[ -n "$ZT_NETWORK_ID" ]]; then
  read -r -p "Approve the Pi in ZeroTier Central (my.zerotier.com), then press Enter..." _
fi

log "Done."
