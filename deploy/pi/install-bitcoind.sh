#!/usr/bin/env bash
# Idempotent Bitcoin Core full-node install for Pi 5 (aarch64).
# Requires: hardened host, SSD mounted at BTC_DATADIR parent (default /mnt/btcdata).
# Does NOT format disks. Skips download if bitcoind already matches BITCOIN_VERSION.
set -euo pipefail

BITCOIN_VERSION="${BITCOIN_VERSION:-31.0}"
BTC_MOUNT="${BTC_MOUNT:-/mnt/btcdata}"
BTC_DATADIR="${BTC_DATADIR:-$BTC_MOUNT/bitcoin}"
RPC_USER="${RPC_USER:-minerva}"
MIN_FREE_GB="${MIN_FREE_GB:-800}"

log() { printf '[bitcoind] %s\n' "$*"; }

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Run as a normal user with sudo, not root." >&2
  exit 1
fi

if ! mountpoint -q "$BTC_MOUNT"; then
  echo "$BTC_MOUNT is not mounted — attach/format SSD and add fstab entry first." >&2
  exit 1
fi

avail_kb=$(df -Pk "$BTC_MOUNT" | awk 'NR==2 {print $4}')
avail_gb=$((avail_kb / 1024 / 1024))
if (( avail_gb < MIN_FREE_GB )); then
  echo "Need at least ${MIN_FREE_GB}GB free on $BTC_MOUNT (found ${avail_gb}GB)." >&2
  exit 1
fi

TS_IP=$(ip -4 -o addr show tailscale0 2>/dev/null | awk '{print $4}' | cut -d/ -f1 || true)
if [[ -z "$TS_IP" ]]; then
  echo "tailscale0 has no IPv4 — start Tailscale before configuring RPC bind." >&2
  exit 1
fi

if ! command -v bitcoind >/dev/null || ! bitcoind --version 2>/dev/null | grep -q "$BITCOIN_VERSION"; then
  log "download Bitcoin Core $BITCOIN_VERSION aarch64"
  cd /tmp
  base="bitcoin-core-${BITCOIN_VERSION}"
  curl -sSLO "https://bitcoincore.org/bin/${base}/bitcoin-${BITCOIN_VERSION}-aarch64-linux-gnu.tar.gz"
  curl -sSLO "https://bitcoincore.org/bin/${base}/SHA256SUMS"
  curl -sSLO "https://bitcoincore.org/bin/${base}/SHA256SUMS.asc"
  # Match only the release tarball (SHA256SUMS also lists *-debug.tar.gz)
  grep -E "bitcoin-${BITCOIN_VERSION}-aarch64-linux-gnu\\.tar\\.gz\$" SHA256SUMS | sha256sum -c -
  tar -xzf "bitcoin-${BITCOIN_VERSION}-aarch64-linux-gnu.tar.gz"
  sudo install -m 0755 -o root -g root -t /usr/local/bin \
    "bitcoin-${BITCOIN_VERSION}/bin/bitcoind" \
    "bitcoin-${BITCOIN_VERSION}/bin/bitcoin-cli"
  rm -rf "bitcoin-${BITCOIN_VERSION}" "bitcoin-${BITCOIN_VERSION}-aarch64-linux-gnu.tar.gz" SHA256SUMS SHA256SUMS.asc
else
  log "bitcoind $BITCOIN_VERSION already installed"
fi

if ! id bitcoin &>/dev/null; then
  sudo useradd --system --home-dir "$BTC_DATADIR" --no-create-home --shell /usr/sbin/nologin bitcoin
fi
sudo mkdir -p "$BTC_DATADIR" /etc/bitcoin
sudo chown bitcoin:bitcoin "$BTC_DATADIR"
sudo chmod 750 "$BTC_DATADIR"

CREDS=/etc/bitcoin/rpc-credentials
AUTHLINE=/etc/bitcoin/rpcauth.line
if [[ ! -f "$CREDS" ]]; then
  log "generating rpcauth (credentials -> $CREDS)"
  sudo python3 - <<PY
import hmac, hashlib, secrets, os
user = "${RPC_USER}"
password = secrets.token_urlsafe(32)
salt = secrets.token_hex(16)
h = hmac.new(salt.encode(), password.encode(), hashlib.sha256).hexdigest()
os.makedirs("/etc/bitcoin", exist_ok=True)
with open("${CREDS}", "w") as f:
    f.write(f"# Bitcoin Core RPC credentials for Minerva Mint\\n")
    f.write(f"rpcuser={user}\\n")
    f.write(f"rpcpassword={password}\\n")
    f.write(f"endpoint=http://${TS_IP}:8332\\n")
os.chmod("${CREDS}", 0o600)
with open("${AUTHLINE}", "w") as f:
    f.write(f"rpcauth={user}:{salt}\${h}\\n")
os.chmod("${AUTHLINE}", 0o600)
print("RPCAUTH_GENERATED")
PY
else
  log "RPC credentials already exist at $CREDS"
fi

if [[ ! -f /etc/bitcoin/bitcoin.conf ]]; then
  log "writing /etc/bitcoin/bitcoin.conf"
  sudo bash -c "cat > /etc/bitcoin/bitcoin.conf" <<EOF
# Minerva Mint full node — mainnet
server=1
daemon=0
txindex=1
prune=0

datadir=${BTC_DATADIR}

dbcache=3000
maxconnections=25

$(sudo cat /etc/bitcoin/rpcauth.line)
rpcbind=127.0.0.1
rpcbind=${TS_IP}
rpcallowip=127.0.0.1
rpcallowip=100.64.0.0/10

listen=0
EOF
  sudo chown root:bitcoin /etc/bitcoin/bitcoin.conf /etc/bitcoin/rpcauth.line
  sudo chmod 640 /etc/bitcoin/bitcoin.conf /etc/bitcoin/rpcauth.line
fi

UNIT=/etc/systemd/system/bitcoind.service
if [[ ! -f "$UNIT" ]]; then
  log "installing systemd unit"
  sudo tee "$UNIT" >/dev/null <<'EOF'
[Unit]
Description=Bitcoin Core daemon (Minerva Mint full node)
After=network-online.target tailscaled.service
Wants=network-online.target
RequiresMountsFor=/mnt/btcdata

[Service]
ExecStart=/usr/local/bin/bitcoind -conf=/etc/bitcoin/bitcoin.conf
User=bitcoin
Group=bitcoin
Type=exec
Restart=on-failure
RestartSec=15
TimeoutStopSec=600

ProtectSystem=full
PrivateTmp=true
NoNewPrivileges=true
MemoryDenyWriteExecute=true
PrivateDevices=true
ProtectHome=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
ReadWritePaths=/mnt/btcdata/bitcoin

[Install]
WantedBy=multi-user.target
EOF
  sudo systemctl daemon-reload
fi

sudo systemctl enable --now bitcoind
log "bitcoind status: $(systemctl is-active bitcoind)"
sudo -u bitcoin bitcoin-cli -conf=/etc/bitcoin/bitcoin.conf -rpcwait getblockchaininfo \
  | grep -E '"chain"|"blocks"|"headers"|"verificationprogress"' || true
log "RPC endpoint: http://${TS_IP}:8332 (credentials in ${CREDS})"
