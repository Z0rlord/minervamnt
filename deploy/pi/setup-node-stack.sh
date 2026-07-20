#!/usr/bin/env bash
# Pi 5 full stack: WiFi -> Tailscale -> Bitcoin Core -> Alby Hub (LDK).
# Run ON the Pi: bash deploy/pi/setup-node-stack.sh
# Or from Mac: ssh -i ~/.ssh/raspi_key ubuntu@<pi-ip> 'bash -s' < deploy/pi/setup-node-stack.sh
set -euo pipefail

REPO_DIR="${REPO_DIR:-$HOME/minervamnt}"
WIFI_SSID="${WIFI_SSID:-SST-WiFi}"

log() { printf '[stack] %s\n' "$*"; }

if [[ "$(uname -m)" != "aarch64" ]]; then
  echo "Run this on the Pi (aarch64), not $(uname -m)" >&2
  exit 1
fi

log "=== 1/5 WiFi ==="
if ! ip route get 8.8.8.8 >/dev/null 2>&1; then
  if [[ -f "$REPO_DIR/deploy/pi/setup-wifi.sh" ]]; then
    bash "$REPO_DIR/deploy/pi/setup-wifi.sh"
  else
    WIFI_SSID="$WIFI_SSID" bash -s <<'WIFI'
set -euo pipefail
WIFI_SSID="${WIFI_SSID:-SST-WiFi}"
WIFI_PASS="${WIFI_PASS:-3445SST2805}"
sudo nmcli device wifi connect "$WIFI_SSID" password "$WIFI_PASS" ifname wlan0 2>/dev/null \
  || { echo "Clone minervamnt repo or set WIFI_PASS"; exit 1; }
WIFI
  fi
else
  log "Network OK: $(ip -4 -o route show default | head -1)"
fi

log "=== 2/5 Tailscale ==="
if ! ip link show tailscale0 >/dev/null 2>&1 || ! tailscale status 2>/dev/null | grep -qE '100\.|fd7a:'; then
  if [[ -x /boot/firmware/install-tailscale.sh ]]; then
    sudo bash /boot/firmware/install-tailscale.sh || true
  else
    curl -fsSL https://tailscale.com/install.sh | sh
    log "Run: sudo tailscale up --auth-key=<key> --hostname=pi5"
  fi
fi
tailscale status 2>/dev/null | head -5 || true

log "=== 3/5 Bitcoin Core ==="
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd || echo "$REPO_DIR/deploy/pi")"
if [[ -f "$SCRIPT_DIR/install-bitcoind.sh" ]]; then
  bash "$SCRIPT_DIR/install-bitcoind.sh"
else
  log "install-bitcoind.sh not found at $SCRIPT_DIR — clone repo to $REPO_DIR"
  exit 1
fi

log "=== 4/5 Alby Hub ==="
if [[ -f "$SCRIPT_DIR/install-albyhub.sh" ]]; then
  bash "$SCRIPT_DIR/install-albyhub.sh"
else
  /bin/bash -c "$(curl -fsSL https://getalby.com/install/hub/pi-aarch64-install.sh)"
fi

log "=== 5/5 Status ==="
systemctl is-active ssh bitcoind tailscaled albyhub 2>/dev/null || true
df -h /mnt/btcdata 2>/dev/null || true
sudo -u bitcoin bitcoin-cli -conf=/etc/bitcoin/bitcoin.conf getblockchaininfo 2>/dev/null \
  | grep -E '"chain"|"blocks"|"verificationprogress"' || log "bitcoind still syncing"
TS_IP=$(ip -4 -o addr show tailscale0 2>/dev/null | awk '{print $4}' | cut -d/ -f1 || true)
log "Tailscale IP: ${TS_IP:-unknown}"
log "Bitcoin RPC (tailnet): http://${TS_IP:-100.75.188.125}:8332"
log "Alby Hub UI: http://pi5.local:8080 (complete setup wizard in browser)"
log "Done."
