#!/usr/bin/env bash
# Install Alby Hub on Pi 5, wired to local bitcoind RPC (full-node stack).
set -euo pipefail

ALBY_DIR="${ALBY_DIR:-/opt/albyhub}"
ALBY_PORT="${ALBY_PORT:-8080}"
RPC_CREDS="${RPC_CREDS:-/etc/bitcoin/rpc-credentials}"

log() { printf '[albyhub] %s\n' "$*"; }

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Run as normal user with sudo, not root." >&2
  exit 1
fi

if ! systemctl is-active --quiet bitcoind; then
  log "WARN: bitcoind not active — start it first (deploy/pi/install-bitcoind.sh)"
fi

if [[ ! -f "$RPC_CREDS" ]]; then
  echo "Missing $RPC_CREDS — run install-bitcoind.sh first" >&2
  exit 1
fi

# shellcheck disable=SC1090
source "$RPC_CREDS"
: "${rpcuser:?}"
: "${rpcpassword:?}"

if [[ ! -x "$ALBY_DIR/bin/albyhub" ]]; then
  log "Installing Alby Hub (official pi-aarch64 script)"
  /bin/bash -c "$(curl -fsSL https://getalby.com/install/hub/pi-aarch64-install.sh)"
fi

log "Configuring systemd for local bitcoind RPC"
sudo mkdir -p "$ALBY_DIR/data"
sudo chown -R "$USER:$USER" "$ALBY_DIR"

sudo tee /etc/systemd/system/albyhub.service >/dev/null <<EOF
[Unit]
Description=Alby Hub (LDK + local bitcoind)
After=network-online.target bitcoind.service
Wants=network-online.target
Requires=bitcoind.service

[Service]
Type=simple
Restart=always
RestartSec=5
User=$USER
ExecStart=$ALBY_DIR/bin/albyhub
CPUQuota=90%

Environment="PORT=$ALBY_PORT"
Environment="WORK_DIR=$ALBY_DIR/data"
Environment="NETWORK=mainnet"
Environment="LDK_BITCOIND_RPC_HOST=127.0.0.1"
Environment="LDK_BITCOIND_RPC_PORT=8332"
Environment="LDK_BITCOIND_RPC_USER=${rpcuser}"
Environment="LDK_BITCOIND_RPC_PASSWORD=${rpcpassword}"
Environment="LDK_LISTENING_ADDRESSES=0.0.0.0:9735"
Environment="LDK_GOSSIP_SOURCE="

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now albyhub

log "albyhub status: $(systemctl is-active albyhub)"
HOST=$(hostname)
log "Open http://${HOST}.local:${ALBY_PORT} or http://127.0.0.1:${ALBY_PORT} to finish wallet setup"
log "Lightning listens on :9735 (forward on router or use Tailscale funnel if needed)"
