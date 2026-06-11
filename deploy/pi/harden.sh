#!/usr/bin/env bash
# Idempotent Pi hardening for Minerva Mint / Bitcoin node host.
# Run on Ubuntu ARM64 (Raspberry Pi 5) as a user with passwordless sudo.
# Safe to re-run. Does NOT format disks or change SSH keys.
set -euo pipefail

log() { printf '[harden] %s\n' "$*"; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Run as a normal user with sudo, not root." >&2
  exit 1
fi

need_cmd sudo

log "apt update && upgrade"
sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get upgrade -y \
  -o Dpkg::Options::="--force-confdef" \
  -o Dpkg::Options::="--force-confold"

SSHD_DROPIN=/etc/ssh/sshd_config.d/99-hardening.conf
if ! sudo grep -q 'PasswordAuthentication no' "$SSHD_DROPIN" 2>/dev/null; then
  log "SSH hardening -> $SSHD_DROPIN"
  printf '%s\n' \
    'PasswordAuthentication no' \
    'KbdInteractiveAuthentication no' \
    'PermitRootLogin no' \
    'X11Forwarding no' \
    'MaxAuthTries 3' \
    'LoginGraceTime 30' \
    'ClientAliveInterval 300' \
    'ClientAliveCountMax 2' \
    | sudo tee "$SSHD_DROPIN" >/dev/null
  sudo sshd -t
  sudo systemctl restart ssh
else
  log "SSH hardening already applied"
fi

if ! command -v ufw >/dev/null; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ufw
fi

if ! ip link show tailscale0 >/dev/null 2>&1; then
  echo "tailscale0 not found — install/start Tailscale before enabling UFW." >&2
  exit 1
fi

log "UFW: tailscale0-only SSH (and optional RPC 8332)"
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw default deny routed
sudo ufw allow in on tailscale0 to any port 22 proto tcp comment 'SSH via Tailscale only' || true
# Bitcoin RPC — only if bitcoind will run on this host
sudo ufw allow in on tailscale0 to any port 8332 proto tcp comment 'bitcoind RPC via Tailscale only' || true
sudo ufw --force enable

if ! command -v fail2ban-client >/dev/null; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq fail2ban
fi

JAIL=/etc/fail2ban/jail.d/sshd-hardening.conf
if [[ ! -f "$JAIL" ]]; then
  log "fail2ban sshd jail"
  printf '%s\n' \
    '[sshd]' \
    'enabled = true' \
    'backend = systemd' \
    'bantime = 1h' \
    'findtime = 10m' \
    'maxretry = 5' \
    | sudo tee "$JAIL" >/dev/null
  sudo systemctl enable --now fail2ban
else
  log "fail2ban jail already present"
fi

if ! command -v unattended-upgrade >/dev/null; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq unattended-upgrades
fi

printf '%s\n' \
  'APT::Periodic::Update-Package-Lists "1";' \
  'APT::Periodic::Unattended-Upgrade "1";' \
  'APT::Periodic::AutocleanInterval "7";' \
  | sudo tee /etc/apt/apt.conf.d/20auto-upgrades >/dev/null
sudo systemctl enable --now unattended-upgrades 2>/dev/null || true

SYSCTL=/etc/sysctl.d/99-hardening.conf
if [[ ! -f "$SYSCTL" ]]; then
  log "sysctl hardening"
  sudo tee "$SYSCTL" >/dev/null <<'EOF'
net.ipv4.conf.all.rp_filter=1
net.ipv4.conf.default.rp_filter=1
net.ipv4.icmp_echo_ignore_broadcasts=1
net.ipv4.conf.all.accept_source_route=0
net.ipv4.conf.default.accept_source_route=0
net.ipv6.conf.all.accept_source_route=0
net.ipv4.conf.all.accept_redirects=0
net.ipv4.conf.default.accept_redirects=0
net.ipv6.conf.all.accept_redirects=0
net.ipv4.conf.all.send_redirects=0
net.ipv4.conf.default.send_redirects=0
net.ipv4.conf.all.log_martians=1
net.ipv4.tcp_syncookies=1
kernel.kptr_restrict=2
kernel.dmesg_restrict=1
EOF
  sudo sysctl -p "$SYSCTL" >/dev/null
fi

for s in avahi-daemon.service avahi-daemon.socket bluetooth.service cups.service cups.socket cups-browsed.service; do
  if systemctl list-unit-files "$s" 2>/dev/null | grep -q enabled; then
    log "disabling $s"
    sudo systemctl disable --now "$s" 2>/dev/null || true
  fi
done

sudo systemctl enable tailscaled 2>/dev/null || true

if ! command -v cloudflared >/dev/null; then
  log "installing cloudflared binary (no tunnel auth)"
  curl -sSL -o /tmp/cloudflared.deb \
    https://github.com/cloudflare/cloudflared/releases/latest/download/cloudflared-linux-arm64.deb
  sudo dpkg -i /tmp/cloudflared.deb >/dev/null
  rm -f /tmp/cloudflared.deb
fi

log "done — verify SSH in a NEW session before closing this one"
sudo ufw status verbose | head -20
