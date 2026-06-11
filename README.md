# Minerva Mint

Ark-backed Cashu mint where issued ecash tokens are backed by Ark VTXOs instead of Lightning liquidity. Public URL: [https://minervamnt.xyz](https://minervamnt.xyz).

> **Current mode: landing page.** The mint API is temporarily disabled. `minervamnt.xyz` serves a static “coming soon” page. See [Landing page mode](#landing-page-mode) to switch modes.

## Architecture

```
┌─────────────┐     ┌──────────────────┐     ┌─────────────┐
│ Cashu Wallet│────▶│  Minerva Mint    │────▶│ Ark Server  │
│  (User)     │     │  - Token issuance│     │   (ASP)     │
└─────────────┘     │  - VTXO inventory│     └─────────────┘
                    │  - Refresh sched.│
                    └────────┬─────────┘
                             │
                    ┌────────┴────────┐
                    │  Bitcoin Core   │
                    │  (Pi 5 / TS)    │
                    └─────────────────┘
```

## Stack

- **Rust** + **axum** HTTP server
- **SQLite** VTXO inventory (`rusqlite`)
- **Mock Ark client** (trait boundary ready for `arkade` / `second`)
- **CDK**: not wired yet — NUT request/response shapes are stubbed; integrate `cdk` 0.16.x when ASP client is ready

## Quick start

```bash
cp .env.example .env
# Set BITCOIN_RPC_PASSWORD from Pi: sudo cat /etc/bitcoin/rpc-credentials

cargo build
cargo test
cargo run
```

Server listens on `0.0.0.0:3338` by default.

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
| GET | `/health` | Reserve, refresh queue, RPC/ASP config; includes `bitcoin` sync info when RPC credentials are set |

## Configuration

`config.toml` plus environment overrides (see `.env.example`):

| Variable | Purpose |
|----------|---------|
| `BITCOIN_RPC_URL` | Pi 5 Bitcoin RPC over Tailscale (`http://100.75.188.125:8332`) |
| `BITCOIN_RPC_USER` | RPC username (`minerva`) |
| `BITCOIN_RPC_PASSWORD` | RPC password — copy from Pi, never commit |
| `MINERVA_CONFIG` | Path to config file (default `config.toml`) |
| `RUST_LOG` | Log filter |

Default `config.toml` points at the Pi Tailscale IP. Override with `.env` for local dev.

## Deployment

### Bitcoin RPC (Raspberry Pi 5 — pi5)

Bitcoin Core 31.0 runs on **pi5** with datadir `/mnt/btcdata/bitcoin` (full node, `txindex=1`).

| Item | Value |
|------|-------|
| Tailscale IP | `100.75.188.125` |
| RPC URL | `http://100.75.188.125:8332` |
| RPC user | `minerva` |
| Password | `/etc/bitcoin/rpc-credentials` on Pi (root-only) |
| SSH | `ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125` |

RPC is bound to the Tailscale interface; UFW allows port `8332` on `tailscale0` only.

**Initial block download takes days.** Check sync from any Tailscale peer:

```bash
curl -s --user minerva:'<password>' \
  --data-binary '{"jsonrpc":"1.0","id":"sync","method":"getblockchaininfo","params":[]}' \
  -H 'content-type: text/plain;' \
  http://100.75.188.125:8332/ \
  | jq '.result | {blocks, headers, verificationprogress, initialblockdownload}'
```

When `.env` has RPC credentials, `GET /health` includes a `bitcoin` object with the same fields.

See [`deploy/pi/README.md`](deploy/pi/README.md) for the full Pi reference.

### Cloudflare Tunnel → minervamnt.xyz

`cloudflared` is installed on pi5. Finish tunnel setup (one-time Cloudflare login required):

1. **Login:** `cloudflared tunnel login` (opens browser for your Cloudflare account)
2. **Create tunnel:** `cloudflared tunnel create minervamnt`
3. **DNS route:** `cloudflared tunnel route dns minervamnt minervamnt.xyz`
4. **Config:** copy [`deploy/cloudflared/config.yml.example`](deploy/cloudflared/config.yml.example) to `~/.cloudflared/config.yml` and replace `<TUNNEL_UUID>`
5. **systemd:** copy [`deploy/systemd/cloudflared.service.example`](deploy/systemd/cloudflared.service.example) to `/etc/systemd/system/cloudflared.service`, then `sudo systemctl enable --now cloudflared`

Ingress (mint mode): `https://minervamnt.xyz` → `http://localhost:3338`.

Ingress (landing mode): `https://minervamnt.xyz` → `http://localhost:8080`.

### Landing page mode

When the mint API should be offline, use the static landing page instead:

```bash
# On pi5 (after git pull)
bash deploy/pi/enable-landing-mode.sh
```

This stops `minerva-mint`, starts `minervamnt-landing` (Python `http.server` on `127.0.0.1:8080`), and points cloudflared at port 8080.

To restore the mint API:

```bash
bash deploy/pi/enable-mint-mode.sh
```

### Minerva Mint systemd

Build release binary on the host, then:

```bash
sudo cp deploy/systemd/minerva-mint.service /etc/systemd/system/
sudo systemctl daemon-reload
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
