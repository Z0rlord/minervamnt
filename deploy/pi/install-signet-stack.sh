#!/usr/bin/env bash
# Install Minerva Mint signet stack on Ubuntu ARM64 (Raspberry Pi).
# Run on the Pi as a user with passwordless sudo after binaries are staged in /opt/minervamnt.
set -euo pipefail

INSTALL_ROOT="${INSTALL_ROOT:-/opt/minervamnt}"
SERVICE_USER="${SERVICE_USER:-minerva}"
BARK_VERSION="${BARK_VERSION:-0.3.0}"
BARK_RELEASE="https://gitlab.com/ark-bitcoin/bark/-/releases/bark-${BARK_VERSION}/downloads"

log() { printf '[signet-install] %s\n' "$*"; }
need_cmd() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Run as a normal user with sudo, not root." >&2
  exit 1
fi
need_cmd sudo

TS_IP="$(ip -4 -o addr show tailscale0 2>/dev/null | awk '{print $4}' | cut -d/ -f1 || true)"
[[ -n "$TS_IP" ]] || {
  echo "tailscale0 has no IPv4 — connect Tailscale before installing signet stack." >&2
  exit 1
}
log "Tailscale IP: $TS_IP"

log "creating service user $SERVICE_USER (if missing)"
if ! id "$SERVICE_USER" &>/dev/null; then
  sudo useradd --system --home "$INSTALL_ROOT" --shell /usr/bin/bash "$SERVICE_USER"
fi

log "installing packages (openssl, curl, jq, protobuf-compiler)"
sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq openssl curl jq ca-certificates protobuf-compiler

sudo mkdir -p "$INSTALL_ROOT"/{bin,data,bark-operator,deploy/systemd}
sudo chown -R "$SERVICE_USER:$SERVICE_USER" "$INSTALL_ROOT"

# Barkd + bark CLI when not already staged by deploy script
if [[ ! -x "$INSTALL_ROOT/bin/barkd" ]]; then
  log "downloading barkd ${BARK_VERSION} (linux-arm64)"
  tmp="$(mktemp)"
  curl -fsSL "$BARK_RELEASE/barkd-${BARK_VERSION}-linux-arm64" -o "$tmp"
  sudo install -m 755 "$tmp" "$INSTALL_ROOT/bin/barkd"
  rm -f "$tmp"
fi
if [[ ! -x "$INSTALL_ROOT/bin/bark" ]]; then
  log "downloading bark ${BARK_VERSION} (linux-arm64)"
  tmp="$(mktemp)"
  curl -fsSL "$BARK_RELEASE/bark-${BARK_VERSION}-linux-arm64" -o "$tmp"
  sudo install -m 755 "$tmp" "$INSTALL_ROOT/bin/bark"
  rm -f "$tmp"
fi

# Config from example (deploy may have already written this)
if [[ ! -f "$INSTALL_ROOT/config.signet.toml" ]]; then
  EXAMPLE="$(dirname "$0")/config.signet.pi.toml.example"
  [[ -f "$EXAMPLE" ]] || EXAMPLE="$INSTALL_ROOT/deploy/pi/config.signet.pi.toml.example"
  [[ -f "$EXAMPLE" ]] || { echo "config.signet.pi.toml.example not found" >&2; exit 1; }
  sed "s/TS_IP/$TS_IP/g" "$EXAMPLE" | sudo tee "$INSTALL_ROOT/config.signet.toml" >/dev/null
fi
sudo chown "$SERVICE_USER:$SERVICE_USER" "$INSTALL_ROOT/config.signet.toml"
sudo chmod 640 "$INSTALL_ROOT/config.signet.toml"

# .env — preserve existing secrets on re-run
ENV_FILE="$INSTALL_ROOT/.env"
if [[ ! -f "$ENV_FILE" ]]; then
  sudo touch "$ENV_FILE"
  sudo chown "$SERVICE_USER:$SERVICE_USER" "$ENV_FILE"
  sudo chmod 600 "$ENV_FILE"
  sudo tee -a "$ENV_FILE" >/dev/null <<EOF
MINERVA_CONFIG=config.signet.toml
BIND_ADDR=${TS_IP}:3338
MINERVA_MINT_URL=http://${TS_IP}:3338
ARK_BACKEND=barkd
BARKD_URL=http://127.0.0.1:3535
SIGNATORY_BACKEND=remote
SIGNATORY_URL=https://127.0.0.1:3340
SIGNATORY_TLS_DIR=${INSTALL_ROOT}/data/cdk-signatory-signet
RUST_LOG=minerva_mint=info,tower_http=info
EOF
fi
sudo chown "$SERVICE_USER:$SERVICE_USER" "$ENV_FILE"
sudo chmod 600 "$ENV_FILE"

# mTLS certs for cdk-signatory (first run)
WORKDIR="$INSTALL_ROOT/data/cdk-signatory-signet"
sudo -u "$SERVICE_USER" mkdir -p "$WORKDIR"
if [[ ! -f "$WORKDIR/ca.pem" ]]; then
  log "generating signatory mTLS certs in $WORKDIR"
  sudo -u "$SERVICE_USER" bash -c "
    set -euo pipefail
    cd '$WORKDIR'
    openssl genrsa -out ca.key 4096
    openssl req -x509 -new -nodes -key ca.key -sha256 -days 825 \
      -subj '/CN=Minerva Signet Signatory CA' -out ca.pem
    cat > server.ext <<'EXT'
subjectAltName=DNS:localhost,IP:127.0.0.1
EXT
    openssl genrsa -out server.key 2048
    openssl req -new -key server.key -out server.csr -subj '/CN=localhost'
    openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key \
      -CAcreateserial -out server.pem -days 825 -sha256 -extfile server.ext
    openssl genrsa -out client.key 2048
    openssl req -new -key client.key -out client.csr -subj '/CN=minerva-mint'
    cat > client.ext <<'EXT'
basicConstraints = CA:FALSE
keyUsage = digitalSignature, keyEncipherment
extendedKeyUsage = clientAuth
EXT
    openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key \
      -CAcreateserial -out client.pem -days 825 -sha256 -extfile client.ext
    rm -f server.csr client.csr server.ext client.ext
    chmod 600 ca.key server.key client.key
  "
fi

# barkd datadir + auth token
sudo -u "$SERVICE_USER" mkdir -p "$INSTALL_ROOT/bark-operator"
if [[ ! -f "$INSTALL_ROOT/bark-operator/auth_token" ]]; then
  log "initializing barkd auth token"
  sudo -u "$SERVICE_USER" "$INSTALL_ROOT/bin/barkd" \
    --datadir "$INSTALL_ROOT/bark-operator" --host 127.0.0.1 --port 3535 &
  barkd_pid=$!
  for _ in $(seq 1 20); do
    [[ -f "$INSTALL_ROOT/bark-operator/auth_token" ]] && break
    sleep 1
  done
  kill "$barkd_pid" 2>/dev/null || true
  wait "$barkd_pid" 2>/dev/null || true
fi

# Inject BARKD_AUTH_TOKEN into .env if missing
if ! sudo grep -q '^BARKD_AUTH_TOKEN=' "$ENV_FILE" 2>/dev/null; then
  token="$(sudo cat "$INSTALL_ROOT/bark-operator/auth_token")"
  echo "BARKD_AUTH_TOKEN=$token" | sudo tee -a "$ENV_FILE" >/dev/null
  sudo chmod 600 "$ENV_FILE"
fi

# Create signet wallet if bark datadir has no wallet yet
if ! sudo -u "$SERVICE_USER" test -f "$INSTALL_ROOT/bark-operator/wallet.db" 2>/dev/null; then
  log "creating signet operator wallet (fund via https://signet.2nd.dev)"
  sudo -u "$SERVICE_USER" env PATH="$INSTALL_ROOT/bin:$PATH" \
    "$INSTALL_ROOT/bin/bark" create --signet \
    --datadir "$INSTALL_ROOT/bark-operator" \
    --ark https://ark.signet.2nd.dev \
    --esplora https://esplora.signet.2nd.dev || {
      log "bark create failed — create wallet manually after barkd is running"
    }
fi

# systemd units
for unit in barkd-signet cdk-signatory-signet minerva-mint-signet; do
  src="$INSTALL_ROOT/deploy/systemd/${unit}.service"
  [[ -f "$src" ]] || src="$(dirname "$0")/../systemd/${unit}.service"
  [[ -f "$src" ]] || { echo "missing unit file: $unit.service" >&2; exit 1; }
  sudo cp "$src" "/etc/systemd/system/${unit}.service"
done
sudo systemctl daemon-reload
sudo systemctl enable barkd-signet cdk-signatory-signet minerva-mint-signet

# Tailscale-only firewall for mint API
if command -v ufw >/dev/null; then
  if ! sudo ufw status 2>/dev/null | grep -q "3338/tcp"; then
    log "UFW: allow mint :3338 on tailscale0 only"
    sudo ufw allow in on tailscale0 to any port 3338 proto tcp comment 'Minerva Mint signet via Tailscale' || true
    sudo ufw --force enable 2>/dev/null || true
  fi
fi

log "starting services"
sudo systemctl restart cdk-signatory-signet
sleep 2
# Bootstrap signatory keyset when empty
if [[ -x "$INSTALL_ROOT/bin/signatory_ping" ]]; then
  sudo -u "$SERVICE_USER" env \
    URL="https://127.0.0.1:3340" \
    TLS_DIR="$WORKDIR" \
    BOOTSTRAP=1 \
    "$INSTALL_ROOT/bin/signatory_ping" || log "signatory bootstrap skipped (may already have keysets)"
fi
sudo systemctl restart barkd-signet
sleep 3
sudo systemctl restart minerva-mint-signet

log "waiting for mint /health"
for _ in $(seq 1 30); do
  if curl -sf "http://127.0.0.1:3338/health" >/dev/null 2>&1 || \
     curl -sf "http://${TS_IP}:3338/health" >/dev/null 2>&1; then
    break
  fi
  sleep 2
done

log "=== status ==="
systemctl is-active barkd-signet cdk-signatory-signet minerva-mint-signet || true
curl -sf "http://${TS_IP}:3338/health" | jq . 2>/dev/null || curl -sf "http://127.0.0.1:3338/health" || true
echo
curl -sf "http://${TS_IP}:3338/v1/info" | jq '{name, pubkey, nuts: .nuts."4"}' 2>/dev/null || true

log "done — mint URL: http://${TS_IP}:3338 (Tailscale only)"
log "Fund operator wallet: bark --datadir $INSTALL_ROOT/bark-operator address"
