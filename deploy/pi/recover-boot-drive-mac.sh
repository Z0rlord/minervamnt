#!/usr/bin/env bash
# Recover Pi 5 boot drive from macOS when SD card is connected via USB reader.
# Run in macOS Terminal (NOT Cursor sandbox): bash deploy/pi/recover-boot-drive-mac.sh
#
# Safe: read-only fsck first, no reformat, preserves /mnt/btcdata on SSD.
set -euo pipefail

BOOT_VOL="/Volumes/system-boot"
ROOT_DEV=""
RECOVERY_ID="pi5-recovery-$(date +%Y%m%d%H%M)"
MOUNT_POINT="/tmp/pi-root"

log() { printf '[recover] %s\n' "$*"; }
die() { printf '[recover] ERROR: %s\n' "$*" >&2; exit 1; }

# bash 3.2 (macOS default) compatible helpers
dev_path() {
  local name="${1#/dev/}"
  printf '/dev/%s' "$name"
}

tolower() {
  printf '%s' "$1" | tr '[:upper:]' '[:lower:]'
}

list_disks() {
  if command -v diskutil >/dev/null 2>&1; then
    diskutil list 2>/dev/null || list_disks_fallback
  else
    list_disks_fallback
  fi
}

list_disks_fallback() {
  log "diskutil unavailable — listing /dev/disk* partitions..."
  ls -la /dev/disk* 2>/dev/null || ls /dev/disk*s* 2>/dev/null || true
}

[[ "$(uname)" == "Darwin" ]] || die "macOS only"

# --- 1. Discover Pi SD card ---
log "Discovering disks..."
list_disks

if [[ ! -d "$BOOT_VOL" ]]; then
  die "Boot volume not mounted at $BOOT_VOL — plug in Pi SD card"
fi

BOOT_DEV=$(mount | awk '$3 == "/Volumes/system-boot" {print $1}')
[[ -n "$BOOT_DEV" ]] || die "Cannot find device for $BOOT_VOL"
# Extract diskN whether mount reports /dev/diskNsM or diskNsM
DISK=$(echo "$BOOT_DEV" | sed -E 's|.*/(disk[0-9]+).*$|\1|')
[[ -n "$DISK" ]] || die "Cannot parse disk id from $BOOT_DEV"
ROOT_PART="${DISK}s2"
ROOT_DEV=$(dev_path "$ROOT_PART")
DISK_DEV=$(dev_path "$DISK")

log "Boot: $BOOT_DEV ($BOOT_VOL)"
log "Root: $ROOT_DEV (ext4, not natively mounted on macOS)"

# --- 2. Install ext4 tools if missing ---
if ! command -v e2fsck >/dev/null 2>&1; then
  log "Installing e2fsprogs..."
  if ! brew install e2fsprogs; then
    die "brew install e2fsprogs failed — fix Homebrew permissions first"
  fi
fi
E2FSCK="$(brew --prefix e2fsprogs)/sbin/e2fsck"
[[ -x "$E2FSCK" ]] || E2FSCK="$(command -v e2fsck || true)"
[[ -n "$E2FSCK" ]] || die "e2fsck not found after install"

# --- 3. Read-only filesystem check ---
log "Running read-only fsck on $ROOT_DEV ..."
sudo "$E2FSCK" -n "$ROOT_DEV" || true

read -r -p "Run fsck repair if errors found? [y/N] " ans
if [[ "$(tolower "$ans")" == "y" ]]; then
  log "Running fsck repair (may take several minutes)..."
  sudo "$E2FSCK" -fy "$ROOT_DEV"
fi

# --- 4. Mount ext4 root ---
sudo mkdir -p "$MOUNT_POINT"
MOUNTED=0

if command -v ext4fuse >/dev/null 2>&1; then
  log "Mounting read-only via ext4fuse..."
  sudo ext4fuse "$ROOT_DEV" "$MOUNT_POINT" -o allow_other,ro && MOUNTED=1
elif [[ -d /Library/Filesystems/macfuse.fs ]]; then
  log "ext4fuse not found; try: brew install --cask macfuse && brew install ext4fuse"
fi

if [[ "$MOUNTED" -eq 0 ]]; then
  log "Cannot mount ext4 — attempting fixes via boot partition cloud-init only"
else
  log "Root mounted at $MOUNT_POINT"

  # --- 5. Diagnose ---
  log "=== /etc/fstab ==="
  cat "$MOUNT_POINT/etc/fstab" 2>/dev/null || true

  log "=== SSH config ==="
  grep -r "" "$MOUNT_POINT/etc/ssh/sshd_config.d/" 2>/dev/null || true
  grep "Include" "$MOUNT_POINT/etc/ssh/sshd_config" 2>/dev/null || true

  log "=== SSH systemd ==="
  ls -la "$MOUNT_POINT/etc/systemd/system/"*ssh* 2>/dev/null || true
  ls -la "$MOUNT_POINT/etc/systemd/system/multi-user.target.wants/"*ssh* 2>/dev/null || true

  # --- 6. Fix fstab (SSD mount must not block boot) ---
  FSTAB="$MOUNT_POINT/etc/fstab"
  if [[ -f "$FSTAB" ]] && grep -q btcdata "$FSTAB"; then
    log "Fixing /etc/fstab btcdata entry..."
    sudo sed -i.bak-recovery \
      -e 's|\(.*btcdata.*\)|\1|' \
      "$FSTAB"
    # Ensure nofail + timeout on non-root mounts
    if grep btcdata "$FSTAB" | grep -qv nofail; then
      sudo sed -i.bak-recovery2 \
        's|\(.* /mnt/btcdata ext4 \)\([^ ]*\)|\1nofail,x-systemd.device-timeout=10|' \
        "$FSTAB" || true
    fi
    log "fstab after fix:"
    grep btcdata "$FSTAB" || true
  fi

  # --- 7. Fix SSH ---
  SSHD_MAIN="$MOUNT_POINT/etc/ssh/sshd_config"
  if [[ -f "$SSHD_MAIN" ]] && ! grep -q 'Include /etc/ssh/sshd_config.d/\*.conf' "$SSHD_MAIN"; then
    log "Adding Include directive to sshd_config..."
    echo "Include /etc/ssh/sshd_config.d/*.conf" | sudo tee -a "$SSHD_MAIN" >/dev/null
  fi

  HARDENING="$MOUNT_POINT/etc/ssh/sshd_config.d/99-hardening.conf"
  if [[ -f "$HARDENING" ]]; then
    log "Validating 99-hardening.conf syntax (basic check)..."
    if grep -qE '^[^#[:space:]]+[[:space:]]+[^#[:space:]]+[[:space:]]+.' "$HARDENING" 2>/dev/null; then
      log "WARN: possible malformed lines in 99-hardening.conf — review manually"
    fi
  fi

  # Remove ssh mask if present
  for mask in ssh.service sshd.service; do
    if [[ -L "$MOUNT_POINT/etc/systemd/system/$mask" ]] || [[ -f "$MOUNT_POINT/etc/systemd/system/$mask" ]]; then
      log "Removing possible ssh mask: $mask"
      sudo rm -f "$MOUNT_POINT/etc/systemd/system/$mask"
    fi
  done

  # Ensure ssh enabled symlink
  WANTS="$MOUNT_POINT/etc/systemd/system/multi-user.target.wants"
  sudo mkdir -p "$WANTS"
  for unit in ssh.service sshd.service; do
    if [[ -f "$MOUNT_POINT/usr/lib/systemd/system/$unit" ]] && [[ ! -e "$WANTS/$unit" ]]; then
      log "Enabling $unit symlink..."
      sudo ln -sf "/usr/lib/systemd/system/$unit" "$WANTS/$unit"
    fi
  done

  # One-shot recovery service for first boot
  RECOVER_SVC="$MOUNT_POINT/etc/systemd/system/pi5-recover-ssh.service"
  if [[ ! -f "$RECOVER_SVC" ]]; then
    log "Installing pi5-recover-ssh.service..."
    sudo tee "$RECOVER_SVC" >/dev/null <<'EOF'
[Unit]
Description=Pi5 one-shot SSH recovery
After=network-online.target
Wants=network-online.target
ConditionPathExists=!/var/lib/pi5-ssh-recovered

[Service]
Type=oneshot
ExecStart=/bin/bash -c 'systemctl unmask ssh sshd 2>/dev/null; systemctl enable ssh 2>/dev/null || systemctl enable sshd; systemctl restart ssh 2>/dev/null || systemctl restart sshd; touch /var/lib/pi5-ssh-recovered'
RemainAfterExit=yes

[Install]
WantedBy=multi-user.target
EOF
    sudo mkdir -p "$MOUNT_POINT/etc/systemd/system/multi-user.target.wants"
    sudo ln -sf /etc/systemd/system/pi5-recover-ssh.service \
      "$MOUNT_POINT/etc/systemd/system/multi-user.target.wants/pi5-recover-ssh.service"
  fi

  log "Unmounting root..."
  sudo umount "$MOUNT_POINT" 2>/dev/null || sudo diskutil unmount "$MOUNT_POINT" 2>/dev/null || true
fi

# --- 8. Boot partition fixes (always) ---
log "Updating boot partition cloud-init for SSH recovery..."

# Backup
sudo cp "$BOOT_VOL/user-data" "$BOOT_VOL/user-data.bak-recovery" 2>/dev/null || true
sudo cp "$BOOT_VOL/cmdline.txt" "$BOOT_VOL/cmdline.txt.bak-recovery" 2>/dev/null || true
sudo cp "$BOOT_VOL/network-config" "$BOOT_VOL/network-config.bak-recovery" 2>/dev/null || true
sudo cp "$BOOT_VOL/meta-data" "$BOOT_VOL/meta-data.bak-recovery" 2>/dev/null || true

# Force cloud-init re-run — preserve kernel root= params (never replace cmdline wholesale)
BASE_CMDLINE=""
if [[ -f "$BOOT_VOL/current/cmdline.txt" ]]; then
  BASE_CMDLINE=$(tr -d '\n' <"$BOOT_VOL/current/cmdline.txt")
elif [[ -f "$BOOT_VOL/cmdline.txt.bak-recovery" ]]; then
  BASE_CMDLINE=$(tr -d '\n' <"$BOOT_VOL/cmdline.txt.bak-recovery" | sed -E 's/(^| )ds=nocloud[^ ]*//g; s/(^| )cfg80211\.ieee80211_regdom=[^ ]*//g')
fi
[[ -n "$BASE_CMDLINE" ]] || BASE_CMDLINE="console=serial0,115200 multipath=off dwc_otg.lpm_enable=0 console=tty1 root=LABEL=writable rootfstype=ext4 panic=10 rootwait fixrtc"

printf '%s\n' "${BASE_CMDLINE} cfg80211.ieee80211_regdom=US ds=nocloud;i=${RECOVERY_ID}" | sudo tee "$BOOT_VOL/cmdline.txt" >/dev/null
printf 'instance-id: %s\n' "$RECOVERY_ID" | sudo tee "$BOOT_VOL/meta-data" >/dev/null

# Remove macOS FAT recovery debris if present
sudo rm -f "$BOOT_VOL"/FSCK*.REC 2>/dev/null || true

sudo tee "$BOOT_VOL/user-data" >/dev/null <<'EOF'
#cloud-config
hostname: pi5

ssh_pwauth: true

users:
  - default
  - name: ubuntu
    ssh_authorized_keys:
      - ssh-ed25519 AAAA...REPLACE_WITH_YOUR_PUBLIC_KEY... comment

packages:
  - openssh-server
  - curl
  - ca-certificates

write_files:
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

  - path: /etc/systemd/system/pi5-recover-ssh.service
    permissions: "0644"
    content: |
      [Unit]
      Description=Pi5 one-shot SSH recovery
      After=network-online.target
      Wants=network-online.target
      ConditionPathExists=!/var/lib/pi5-ssh-recovered
      [Service]
      Type=oneshot
      ExecStart=/bin/bash -c 'systemctl unmask ssh sshd 2>/dev/null; systemctl enable ssh 2>/dev/null || systemctl enable sshd; systemctl start ssh 2>/dev/null || systemctl start sshd; touch /var/lib/pi5-ssh-recovered'
      RemainAfterExit=yes
      [Install]
      WantedBy=multi-user.target

bootcmd:
  - [ bash, -lc, "chmod +x /boot/firmware/install-tailscale.sh" ]

runcmd:
  - [ systemctl, unmask, ssh ]
  - [ systemctl, enable, ssh ]
  - [ systemctl, start, ssh ]
  - [ systemctl, enable, pi5-recover-ssh.service ]
  - [ systemctl, start, pi5-recover-ssh.service ]
  - [ systemctl, daemon-reload ]
  - [ systemctl, enable, join-tailscale.service ]
  - [ systemctl, start, join-tailscale.service ]
EOF

log "Boot partition updated (instance-id: $RECOVERY_ID)"

# --- 9. Safely eject ---
log "Syncing and unmounting..."
sync
read -r -p "Unmount SD card now? [Y/n] " eject_ans
if [[ "$(tolower "$eject_ans")" != "n" ]]; then
  if command -v diskutil >/dev/null 2>&1; then
    sudo diskutil unmountDisk "$DISK_DEV"
  else
    sudo umount "$DISK_DEV" 2>/dev/null || true
  fi
  log "SD card unmounted. Reinsert in Pi 5 and boot."
fi

cat <<EOF

=== NEXT STEPS ===
1. Reinsert SD card in Pi 5 (SSD can stay attached to Pi — Bitcoin data preserved)
2. Power on Pi, wait 2-3 min for Wi-Fi + Tailscale
3. Test SSH:
   ssh -i ~/.ssh/your_key user@YOUR_HOST
4. Verify services:
   systemctl is-active ssh bitcoind tailscaled
   df -h /mnt/btcdata
   sudo -u bitcoin bitcoin-cli -conf=/etc/bitcoin/bitcoin.conf getblockchaininfo | head -5
5. If SSH works, deploy landing page (set DEPLOY_HOST first):
   export DEPLOY_HOST=user@YOUR_HOST
   bash deploy/pi/deploy-landing-from-mac.sh

SSD UUID expected in fstab: e59157e3-3014-44f9-afa5-0e3d02f39a8d -> /mnt/btcdata
EOF
