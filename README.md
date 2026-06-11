# Minerva Mint

[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org/)
[![License: GPL-3.0](https://img.shields.io/badge/License-GPL--3.0-blue.svg)](LICENSE)

**Ark-backed Cashu mint** — Chaumian ecash where issued tokens are backed by
[Ark](https://ark-protocol.org) VTXOs instead of Lightning channel liquidity.
Users get Cashu-style privacy; backing sits in unilateral-exit-capable VTXOs
rather than a hot Lightning node.

> **⚠️ Legal notice**
>
> Operating a mint may constitute regulated financial activity in many
> jurisdictions. This is **experimental software** with **no warranty**.
> See [docs/DISCLAIMER.md](docs/DISCLAIMER.md) before deploying for others.

> **Status: active scaffold.** End-to-end flows run against a mock ASP and mock
> BDHKE. Production paths (real ASP client, CDK signatory, live Bitcoin RPC)
> are designed behind traits and documented in the roadmap below.

## Why this exists

Most Cashu mints today custody bitcoin in Lightning — channel management,
liquidity balancing, and node uptime become the operator's problem. Minerva
Mint explores a different backing model:

| Layer | Role |
| ----- | ---- |
| **Cashu** | Chaumian ecash (NUT HTTP API, blind signatures) |
| **Ark VTXOs** | Off-chain UTXOs with unilateral exit to L1 |
| **Bitcoin Core** | Chain truth, fee estimation, exit broadcast |
| **Transparency** | PoL + PoR + OpenTimestamps — inflation detectable, not invisible |

The goal is **freedom-tech infrastructure**: ecash issuance and redemption
where no single vendor controls the full stack — open protocols, auditable
liabilities, and an exit path that does not depend on the mint staying online.

## Sovereign Engineering Cohort (SEC-08)

This project is being developed for **[SEC-08](https://sovereignengineering.io)**
(July 20 – August 28, 2026, Madeira) — six weeks of open exploration with
builders working on protocols and tools no single entity can control.

**What we bring:** a working Rust scaffold with NUT routes, VTXO inventory,
signatory policy gates, Proof of Liabilities epochs (OpenTimestamps-anchored),
and V-PACK verification hooks.

**What we want to ship at cohort:**

- Live **Ark ASP** integration (`arkade` / `second`) behind the `ArkClient` trait
- **CDK** BDHKE + remote **signatory** (keys off the API process)
- Operator-run **transparency dashboard** from `/transparency/*` endpoints
- Documented path for **user-held V-PACK** exit material (trust-minimized mode)
- Real-world test mint on testnet/signet with public PoL roots

If you are building adjacent freedom tech (Cashu, Ark, ecash wallets, audit
tools) and heading to Madeira, we'd love to collaborate.

→ [Apply to SEC-08 (YOLO++)](https://sovereignengineering.io) · [sovereignengineering.io](https://sovereignengineering.io)

## Architecture

```
┌─────────────┐     ┌──────────────────────────┐     ┌─────────────┐
│ Cashu       │────▶│  Minerva Mint (Rust)     │────▶│ Ark ASP     │
│ wallet      │     │  · NUT API (axum)        │     │             │
└─────────────┘     │  · SignatoryPolicy       │     └─────────────┘
                    │  · VTXO inventory (SQL)  │
                    │  · PoL ledger + OTS      │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────┴─────────────┐
                    │  Bitcoin Core (your node) │
                    │  sync · fees · broadcast  │
                    └──────────────────────────┘
```

### Token lifecycle

1. **Mint** — User pays (bolt11 via ASP) → mint issues blinded tokens → batch
   mapped to a backing VTXO. Auto-boards sats when free reserve is low.
2. **Swap** — Standard Cashu refresh; no Ark interaction.
3. **Melt** — Proofs burned → payout through ASP (LN or Ark offboarding).
4. **Refresh** — Scheduler rolls VTXOs before expiry; mappings updated atomically.
5. **Exit** — `POST /ark/exit` broadcasts unilateral exit txs if the ASP fails.

Details: [docs/trust-model.md](docs/trust-model.md)

## Quick start

Requirements: Rust stable, SQLite (bundled via `rusqlite`).

```bash
git clone https://github.com/Z0rlord/minervamnt.git
cd minervamnt

cp .env.example .env   # set secrets locally — never commit .env
cargo build
cargo test             # unit + HTTP integration tests
cargo run              # listens on 0.0.0.0:3338 by default
```

Smoke test:

```bash
curl -s localhost:3338/health | jq
curl -s localhost:3338/v1/info | jq
curl -s localhost:3338/transparency/summary | jq
```

Out of the box the mint uses a **deterministic mock ASP** — no external
services required for development.

## Configuration

Non-secret defaults live in `config.toml`. Secrets and overrides use
environment variables (see `.env.example`).

| Section | Keys | Purpose |
| ------- | ---- | ------- |
| `[mint]` | `name`, `url`, `description` | Identity on `/v1/info` |
| `[server]` | `host`, `port` | HTTP bind address |
| `[ark]` | `server_url`, `server_pubkey`, expiry blocks | ASP connection |
| `[bitcoin]` | `rpc_url` (+ env user/password) | Your Bitcoin Core RPC |
| `[liquidity]` | reserve / board thresholds | VTXO sizing policy |
| `[database]` | `path` | SQLite file location |
| `[trust]` | `vtxo_verify_mode`, `pol_enabled`, … | Verification & PoL |
| `[trust.ots]` | `enabled`, `calendar_urls` | OpenTimestamps stamping |

Common env vars: `BITCOIN_RPC_URL`, `BITCOIN_RPC_USER`, `BITCOIN_RPC_PASSWORD`,
`MINERVA_CONFIG`, `MINERVA_MINT_URL`, `RUST_LOG`.

**Your infrastructure, your rules.** Run Bitcoin Core on any host you control
(home server, VPS, colo). Reach it however you prefer — localhost, VPN, SSH
tunnel, or private network. The mint only needs a standard JSON-RPC endpoint.

## API

### Cashu (NUT)

| Method | Path | NUT |
| ------ | ---- | --- |
| GET | `/v1/info` | NUT-06 |
| POST | `/v1/mint/quote/bolt11` | NUT-04 |
| POST | `/v1/mint/bolt11` | NUT-04 |
| POST | `/v1/melt/quote/bolt11` | NUT-05 |
| POST | `/v1/melt/bolt11` | NUT-05 |
| POST | `/v1/swap` | NUT-03 |

### Ark extensions

| Method | Path | Purpose |
| ------ | ---- | ------- |
| GET | `/ark/vtxo/{token_id}` | Backing VTXO for an issuance batch |
| POST | `/ark/exit` | Unilateral exit for a token's VTXO |
| GET | `/ark/refresh/status` | Refresh queue and expiry horizon |

### Transparency & operations

| Method | Path | Purpose |
| ------ | ---- | ------- |
| GET | `/transparency/summary` | Ecash vs VTXO backing, PoL/OTS status |
| GET | `/v1/pol/status` | Current PoL epoch totals |
| GET | `/v1/pol/roots/{keyset_id}` | Closed epoch Merkle roots |
| GET | `/v1/pol/ots/{epoch_day}` | OpenTimestamps proof for an epoch |
| GET | `/health` | ASP connectivity, reserve, optional Bitcoin sync |

## Trust model (summary)

Minerva implements **dual attestation**:

- **Proof of Liabilities (PoL)** — every mint and melt logged; daily epochs with
  chained Merkle roots, anchored via [OpenTimestamps](https://opentimestamps.org/).
- **Proof of Reserves (PoR)** — outstanding ecash reconciled against allocated
  VTXO msat (`GET /transparency/summary`).
- **Signatory policy** — blind signing gated on paid quote + VTXO allocation.
- **V-PACK verification** — structural checks on boarded VTXOs (`scaffold` or
  `vpack` mode via [libvpack-rs](https://github.com/jgmcalpine/libvpack-rs)).

Full spec: [docs/trust-model.md](docs/trust-model.md)

## Production deployment (operator guide)

This repo ships **reference material** under `deploy/` — adapt to your environment:

| Path | Contents |
| ---- | -------- |
| `deploy/systemd/` | Example systemd unit for the mint binary |
| `deploy/cloudflared/` | Example Cloudflare Tunnel config (optional) |
| `deploy/cloudflare-pages/` | Static landing page deploy notes |
| `deploy/pi/` | Example bare-metal / edge notes (optional reference) |

Typical production layout:

1. Build release binary: `cargo build --release`
2. Run behind reverse proxy or tunnel with TLS termination
3. Point `[bitcoin].rpc_url` at your synced node (mainnet or signet)
4. Connect a real Ark ASP; set `[trust] vtxo_verify_mode = "vpack"`
5. Run **CDK signatory** on a separate host; disable mint-side signing keys
6. Expose `/transparency/summary` publicly for third-party reconciliation

Never commit RPC passwords, ASP keys, or tunnel tokens to git.

## Roadmap

| Milestone | Status |
| --------- | ------ |
| NUT HTTP scaffold + mock ASP | Done |
| VTXO inventory + refresh scheduler | Done |
| SignatoryPolicy + PoL epochs + OTS | Done |
| V-PACK verify at insert | Done |
| Real Ark ASP client | SEC-08 target |
| CDK BDHKE + remote signatory | SEC-08 target |
| NUT-20 signed quotes | Planned |
| User-delivered V-PACK at mint | Planned |
| PostgreSQL backend option | Planned |

## Open questions

Contributions and design discussion welcome on these:

1. Who pays Ark refresh fees — operator or users?
2. Mint-held vs user-held unilateral exit proofs?
3. Redemption policy when a VTXO expires before melt?
4. Dual backend (Ark + LN gateway) or Ark-only melts?
5. VTXO granularity — few large vs many small VTXOs?

## Contributing

Issues and PRs welcome. Please do not commit secrets or operator-specific
infrastructure details (hostnames, IPs, credentials) into the public tree.

```bash
cargo fmt
cargo clippy -- -D warnings
cargo test
```

## License

GNU General Public License v3.0 or later — see [LICENSE](LICENSE).

Regulatory disclosures: [docs/DISCLAIMER.md](docs/DISCLAIMER.md)

## Links

- [Cashu NUTs](https://github.com/cashubtc/nuts)
- [Cashu CDK](https://github.com/cashubtc/cdk)
- [Ark protocol](https://ark-protocol.org/)
- [PoL specification](https://gist.github.com/victorandre957/4f497d385e1fd9a47898480903f56b3e)
- [Sovereign Engineering](https://sovereignengineering.io)
