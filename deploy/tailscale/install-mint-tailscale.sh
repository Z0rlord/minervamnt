#!/usr/bin/env bash
# Install Minerva Mint on a VPS / Pi and expose it over Tailscale only.
#
# Reference installer — review before running on production hardware. It runs the
# stock mint (whatever `MINERVA_CONFIG` points at; default `config.toml` = mock
# ASP + mock signatory). For the barkd/cdk-signatory signet operator stack use
# deploy/pi/install-signet-stack.sh instead.
#
# Prereqs on the host:
#   - Tailscale installed and connected (`tailscale up`), so `tailscale0` exists.
#   - A release binary staged at $INSTALL_ROOT/bin/minerva-mint, OR run with
#     BUILD_FROM_SRC=1 in a checkout with the Rust toolchain available.
#
# Usage (on the host, as a normal user with passwordless sudo):
#   bash deploy/tailscale/install-mint-tailscale.sh
#   BUILD_FROM_SRC=1 bash deploy/tailscale/install-mint-tailscale.sh
set -euo pipefail

INSTALL_ROOT="${INSTALL_ROOT:-/opt/minervamnt}"
SERVICE_USER="${SERVICE_USER:-minerva}"
MINT_PORT="${MINT_PORT:-3338}"
BUILD_FROM_SRC="${BUILD_FROM_SRC:-0}"

log() { printf '[ts-install] %s\n' "$*"; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Run as a normal user with sudo, not root." >&2
  exit 1
fi
need_cmd sudo

# Derive the Tailscale IPv4 (bind the mint here so it is never on LAN/public).
TS_IP="$(tailscale ip -4 2>/dev/null | head -n1 || true)"
[[ -n "$TS_IP" ]] || TS_IP="$(ip -4 -o addr show tailscale0 2>/dev/null | awk '{print $4}' | cut -d/ -f1 || true)"
[[ -n "$TS_IP" ]] || {
  echo "tailscale0 has no IPv4 — run 'tailscale up' before installing." >&2
  exit 1
}
log "Tailscale IP: $TS_IP"

log "creating service user $SERVICE_USER (if missing)"
if ! id "$SERVICE_USER" &>/dev/null; then
  sudo useradd --system --home "$INSTALL_ROOT" --shell /usr/bin/bash "$SERVICE_USER"
fi

sudo mkdir -p "$INSTALL_ROOT"/{bin,data,deploy/systemd}
sudo chown -R "$SERVICE_USER:$SERVICE_USER" "$INSTALL_ROOT"

# Binary: build from source (this checkout) or expect a staged release binary.
if [[ "$BUILD_FROM_SRC" == "1" ]]; then
  need_cmd cargo
  log "building release binary from source"
  cargo build --release
  sudo install -m 755 target/release/minerva-mint "$INSTALL_ROOT/bin/minerva-mint"
fi
[[ -x "$INSTALL_ROOT/bin/minerva-mint" ]] || {
  echo "no binary at $INSTALL_ROOT/bin/minerva-mint — stage a release build or re-run with BUILD_FROM_SRC=1." >&2
  exit 1
}

# config.toml — copy repo default if not already present (edit for production).
if [[ ! -f "$INSTALL_ROOT/config.toml" ]]; then
  SRC_CFG="$(dirname "$0")/../../config.toml"
  [[ -f "$SRC_CFG" ]] || { echo "config.toml not found next to repo" >&2; exit 1; }
  sudo cp "$SRC_CFG" "$INSTALL_ROOT/config.toml"
fi
sudo chown "$SERVICE_USER:$SERVICE_USER" "$INSTALL_ROOT/config.toml"

# .env — preserve on re-run; bind to the Tailscale IP only.
ENV_FILE="$INSTALL_ROOT/.env"
if [[ ! -f "$ENV_FILE" ]]; then
  sudo tee "$ENV_FILE" >/dev/null <<EOF
MINERVA_CONFIG=config.toml
BIND_ADDR=${TS_IP}:${MINT_PORT}
MINERVA_MINT_URL=http://${TS_IP}:${MINT_PORT}
RUST_LOG=minerva_mint=info,tower_http=info
EOF
fi
sudo chown "$SERVICE_USER:$SERVICE_USER" "$ENV_FILE"
sudo chmod 600 "$ENV_FILE"

# systemd unit (generic mainnet/default unit reads $INSTALL_ROOT/.env).
UNIT_SRC="$(dirname "$0")/../systemd/minerva-mint.service"
[[ -f "$UNIT_SRC" ]] || { echo "missing unit: minerva-mint.service" >&2; exit 1; }
sed "s/<USER>/$SERVICE_USER/g" "$UNIT_SRC" \
  | sed "s#ExecStart=.*#ExecStart=$INSTALL_ROOT/bin/minerva-mint#" \
  | sudo tee /etc/systemd/system/minerva-mint.service >/dev/null
sudo systemctl daemon-reload
sudo systemctl enable minerva-mint

# Tailscale-only firewall for the mint API.
if command -v ufw >/dev/null; then
  if ! sudo ufw status 2>/dev/null | grep -q "${MINT_PORT}/tcp"; then
    log "UFW: allow mint :${MINT_PORT} on tailscale0 only"
    sudo ufw allow in on tailscale0 to any port "$MINT_PORT" proto tcp comment 'Minerva Mint via Tailscale' || true
    sudo ufw --force enable 2>/dev/null || true
  fi
fi

log "starting minerva-mint"
sudo systemctl restart minerva-mint

log "waiting for mint /health"
for _ in $(seq 1 30); do
  curl -sf "http://${TS_IP}:${MINT_PORT}/health" >/dev/null 2>&1 && break
  sleep 2
done

log "=== status ==="
systemctl is-active minerva-mint || true
curl -sf "http://${TS_IP}:${MINT_PORT}/health" | jq . 2>/dev/null || true
echo
curl -sf "http://${TS_IP}:${MINT_PORT}/v1/info" | jq '{name, nuts: (.nuts|keys)}' 2>/dev/null || true

log "done — mint URL: http://${TS_IP}:${MINT_PORT} (Tailscale only)"
log "Optional HTTPS + MagicDNS: bash deploy/tailscale/serve-https.sh"
