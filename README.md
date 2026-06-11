# Minerva Mint

Ark-backed Cashu mint where issued ecash tokens are backed by Ark VTXOs instead of Lightning liquidity. Public URL: [https://minervamnt.xyz](https://minervamnt.xyz).

> **Canonical repo.** This directory (`~/Projects/minervamnt`) is the GitHub source of truth ([Z0rlord/minervamnt](https://github.com/Z0rlord/minervamnt)). A parallel scaffold at `~/Projects/minerva-mint` was created during early bootstrapping and is **deprecated/spare** — do not develop there; merge any stray changes into this repo instead.

> **Current mode: landing page (Cloudflare Pages).** The mint API and Pi tunnel are disabled. `minervamnt.xyz` is served as static HTML from Cloudflare Pages — independent of the Pi. See [Landing page (Cloudflare Pages)](#landing-page-cloudflare-pages).

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

### Landing page (Cloudflare Pages)

`minervamnt.xyz` is deployed to **Cloudflare Pages** from the `landing/` directory. No Pi or tunnel is required for the public site.

| Item | Value |
|------|-------|
| Pages project | `minervamnt` |
| Deploy | [`deploy/cloudflare-pages/README.md`](deploy/cloudflare-pages/README.md) |
| DNS | `minervamnt.xyz` / `www` → CNAME `minervamnt.pages.dev` |

```bash
# Manual deploy from Mac (Doppler injects CLOUDFLARE_API_TOKEN)
doppler run --project dojopop --config prd_zorie -- bash -c '
  export CLOUDFLARE_ACCOUNT_ID=dfc6e38d5b254f0f8ffac8a0e554112a
  npx wrangler@4 pages deploy landing --project-name=minervamnt --branch=main
'
```

Pushes to `main` that touch `landing/**` also deploy via GitHub Actions (requires `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID` repo secrets).

### Cloudflare Tunnel (Pi — disabled for apex)

`cloudflared` remains installed on pi5 but **DNS no longer points at the tunnel**. Apex traffic goes to Pages. The tunnel can stay stopped; the site works even when Pi SSH is down.

To restore the mint API later, use a subdomain (e.g. `api.minervamnt.xyz` → tunnel → `:3338`) or switch DNS back to the tunnel. Pi-side helpers are still in [`deploy/pi/`](deploy/pi/):

```bash
# On pi5 — only when serving mint/landing via tunnel again
bash deploy/pi/enable-mint-mode.sh      # mint API on :3338
bash deploy/pi/enable-landing-mode.sh   # static landing on :8080 via tunnel
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
