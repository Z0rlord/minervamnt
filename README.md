# Minerva Mint

Ark-backed Cashu mint where issued ecash tokens are backed by Ark VTXOs instead of Lightning liquidity. Public URL: [https://minervamnt.xyz](https://minervamnt.xyz).

## Architecture

```
┌─────────────┐     ┌──────────────────┐     ┌─────────────┐
│ Cashu Wallet│────▶│  Minerva Mint    │────▶│ Ark Server  │
│  (User)     │     │  - Token issuance│     │   (ASP)     │
└─────────────┘     │  - VTXO inventory│     └─────────────┘
                    │  - Refresh sched.│
                    └────────┬─────────┘
                             │ Tailscale RPC (100.64.0.0/10)
                    ┌────────┴────────┐
                    │  Bitcoin Core   │
                    │  pi5 / TS only  │
                    └─────────────────┘
                             ▲
                    Cloudflare Tunnel (public HTTPS only)
```

## Stack

- **Rust** + **axum** HTTP server
- **SQLite** VTXO inventory (`rusqlite`)
- **Mock Ark client** (trait boundary ready for `arkade` / `second`)
- **CDK**: not wired yet — NUT request/response shapes are stubbed; integrate `cdk` 0.16.x when ASP client is ready

## Quick start

```bash
cp .env.example .env
# Set BITCOIN_RPC_PASSWORD from Pi:
# ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125 'sudo grep rpcpassword /etc/bitcoin/rpc-credentials'

cargo build
cargo test
cargo run
```

Server listens on `0.0.0.0:3338` by default.

Default Bitcoin RPC points at the Pi 5 Tailscale address (`100.75.188.125:8332`). Override via `.env` if needed.

## API

### Cashu NUT endpoints (stubs)

| Method | Path | Description |
|--------|------|-------------|
| GET | `/v1/info` | Mint info and supported NUTs |
| POST | `/v1/mint/quote/bolt11` | Request mint quote |
| GET | `/v1/mint/quote/bolt11/{quote_id}` | Quote state |
| POST | `/v1/mint/bolt11` | Issue tokens |
| POST | `/v1/melt/quote/bolt11` | Melt quote |
| POST | `/v1/melt/bolt11` | Redeem tokens |
| POST | `/v1/swap` | Swap tokens |

### Ark extensions

| Method | Path | Description |
|--------|------|-------------|
| GET | `/ark/vtxo/{token_id}` | VTXO proof for unilateral exit |
| POST | `/ark/exit` | Initiate unilateral exit |
| GET | `/ark/refresh/status` | Pending refresh queue |

### Operations

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Reserve, refresh queue, Bitcoin sync probe, RPC/ASP config |

## Configuration

`config.toml` plus environment overrides (see `.env.example`):

| Variable | Purpose |
|----------|---------|
| `BITCOIN_RPC_URL` | Pi 5 Bitcoin RPC over Tailscale (`http://100.75.188.125:8332`) |
| `BITCOIN_RPC_USER` | RPC username (`minerva`) |
| `BITCOIN_RPC_PASSWORD` | RPC password (from Pi credentials file) |
| `MINERVA_CONFIG` | Path to config file (default `config.toml`) |
| `RUST_LOG` | Log filter |

## Deployment

### Raspberry Pi 5 — hardening checklist

Production Bitcoin node host: **pi5** (`100.75.188.125` on Tailscale, `pi5.tailbcc07.ts.net`).

| Step | Status / command |
|------|------------------|
| Tailscale on boot | `systemctl is-enabled tailscaled` |
| SSH key-only | `deploy/pi/harden.sh` → `/etc/ssh/sshd_config.d/99-hardening.conf` |
| UFW default deny | SSH + RPC **only** on `tailscale0`; no public 8333/8332 |
| fail2ban sshd | bantime 1h, maxretry 5 |
| unattended-upgrades | daily security patches |
| sysctl hardening | `/etc/sysctl.d/99-hardening.conf` |
| cloudflared binary | installed; tunnel auth is manual (below) |
| 1TB SSD | `/mnt/btcdata` ext4, Bitcoin datadir `/mnt/btcdata/bitcoin` |
| Bitcoin Core 31.0 | `deploy/pi/install-bitcoind.sh` (idempotent) |

Re-run scripts on the Pi (after `git clone` or `scp`):

```bash
chmod +x deploy/pi/*.sh
./deploy/pi/harden.sh          # safe to re-run
./deploy/pi/install-bitcoind.sh # requires mounted SSD with 800GB+ free
```

Operational reference: `deploy/pi/README.md`.

**SSH:** `ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125`

### Tailscale RPC wiring (mint → Pi)

1. Mint host (Mac, VPS, or same Pi) joins the same tailnet.
2. `.env`: `BITCOIN_RPC_URL=http://100.75.188.125:8332`, user `minerva`, password from Pi:
   ```bash
   ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125 \
     'sudo grep rpcpassword /etc/bitcoin/rpc-credentials'
   ```
3. RPC is bound to the Pi Tailscale IP; `rpcallowip=100.64.0.0/10` — **never** expose 8332 on the public internet.
4. Check sync from any Tailscale peer:
   ```bash
   curl -s --user minerva:<password> \
     --data-binary '{"jsonrpc":"1.0","id":"sync","method":"getblockchaininfo","params":[]}' \
     -H 'content-type: text/plain;' \
     http://100.75.188.125:8332 \
     | jq '.result | {chain, blocks, headers, verificationprogress, initialblockdownload}'
   ```
5. `/health` reports chain, block height, and sync state when RPC credentials are in `.env`.

**Initial block download (IBD) takes days** on a Pi. The mint can run during sync, but Bitcoin-backed features need `initialblockdownload: false` and `verificationprogress` ≈ 1.

### ZeroTier (optional)

Tailscale is the primary overlay. ZeroTier can run **alongside** Tailscale for an alternate mesh path; do not expose Bitcoin RPC on either interface to the public WAN. If you use ZeroTier, add UFW rules only for the ZeroTier interface (`zt*`) mirroring the `tailscale0` rules, or route RPC exclusively over one overlay to reduce attack surface.

### Cloudflare Tunnel → minervamnt.xyz

Public ingress for the mint HTTP API only (port 3338). Bitcoin RPC stays off the tunnel.

1. On the mint host, install `cloudflared` (Pi already has the binary).
2. Authenticate (one-time, browser):
   ```bash
   cloudflared tunnel login
   ```
3. Create tunnel and DNS route:
   ```bash
   cloudflared tunnel create minervamnt
   cloudflared tunnel route dns minervamnt minervamnt.xyz
   ```
4. Copy and edit config:
   ```bash
   sudo mkdir -p /etc/cloudflared
   sudo cp deploy/cloudflared/config.yml.example /etc/cloudflared/config.yml
   # Set <TUNNEL_ID> and credentials path
   sudo cp ~/.cloudflared/<TUNNEL_ID>.json /etc/cloudflared/
   ```
5. Enable systemd (adjust user/paths):
   ```bash
   sudo cp deploy/systemd/cloudflared.service.example /etc/systemd/system/cloudflared.service
   sudo systemctl enable --now cloudflared
   ```
6. Ingress: `https://minervamnt.xyz` → `http://localhost:3338` (see `deploy/cloudflared/config.yml.example`).

### Minerva Mint systemd service

```bash
sudo useradd --system --create-home minerva || true
sudo mkdir -p /opt/minervamnt/data
# build release binary, copy .env from .env.example
sudo cp deploy/systemd/minerva-mint.service /etc/systemd/system/
sudo systemctl enable --now minerva-mint
```

## Open questions

- Who pays VTXO refresh fees — operator or users?
- Does the user hold the VTXO proof, or only the mint?
- What happens if a VTXO expires before token redemption?
- Lightning gateway: dual backend or pure Ark?
- VTXO amount granularity — match denominations or allow splitting?

## Deferred

- Real `cdk-mintd` / `cdk` integration
- Live Ark ASP client (`arkade` / `second`)
- PostgreSQL for production
- Prometheus metrics and operator alerts

## License

MIT
