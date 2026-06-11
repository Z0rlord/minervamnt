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
# Edit BITCOIN_RPC_URL to your Pi 5 Tailscale IP

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
| GET | `/health` | Reserve, refresh queue, RPC/ASP config |

## Configuration

`config.toml` plus environment overrides (see `.env.example`):

| Variable | Purpose |
|----------|---------|
| `BITCOIN_RPC_URL` | Pi 5 Bitcoin RPC over Tailscale, e.g. `http://100.x.x.x:8332` |
| `BITCOIN_RPC_USER` | RPC username |
| `BITCOIN_RPC_PASSWORD` | RPC password |
| `MINERVA_CONFIG` | Path to config file (default `config.toml`) |
| `RUST_LOG` | Log filter |

## Deployment

### Cloudflare Tunnel → minervamnt.xyz

1. Install `cloudflared` on the mint host.
2. `cloudflared tunnel create minervamnt`
3. Route DNS: `minervamnt.xyz` → tunnel.
4. Ingress: `https://minervamnt.xyz` → `http://localhost:3338`.

### Bitcoin RPC (Raspberry Pi 5)

Another node on your Tailscale network runs Bitcoin Core. Bind RPC to the Tailscale interface only (`rpcbind=100.x.x.x`, `rpcallowip=100.64.0.0/10`). Set credentials in `.env` — never commit secrets.

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
