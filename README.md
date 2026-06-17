# Minerva Mint

[![Rust](https://img.shields.io/badge/rust-2021-orange.svg)](https://www.rust-lang.org/)
[![License: AGPL-3.0](https://img.shields.io/badge/License-AGPL--3.0-blue.svg)](LICENSE)

**Ark-backed Cashu mint** — Chaumian ecash where issued tokens are backed by
[Ark](https://ark-protocol.org) VTXOs instead of Lightning channel liquidity.
Users get Cashu-style privacy; backing sits in unilateral-exit-capable VTXOs
rather than a hot Lightning node.

> **⚠️ Legal notice**
>
> Operating a mint may constitute regulated financial activity in many
> jurisdictions. This is **experimental software** with **no warranty**.
> See [docs/DISCLAIMER.md](docs/DISCLAIMER.md) before deploying for others.

> **Status: active scaffold.** Default dev mode uses mock ASP + mock BDHKE.
> Production-shaped paths are wired: **barkd** and **Arkade** ASP clients,
> **cdk-signatory** remote signing, optional **exit auto-claim**, and **live
> melt payout** when `ark.backend = barkd`. `/v1/info` pubkey metadata remains
> scaffolded.

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
| `[ark]` | `backend`, `server_url`, `barkd_url` / `wallet_url`, exit claim | ASP + wallet daemon |
| `[signatory]` | `backend`, `url` | BDHKE signing (mock / remote / local) |
| `[bitcoin]` | `rpc_url` (+ env user/password) | Your Bitcoin Core RPC |
| `[liquidity]` | reserve / board thresholds | VTXO sizing policy |
| `[database]` | `path` | SQLite file location |
| `[trust]` | `vtxo_verify_mode`, `pol_enabled`, … | Verification & PoL |
| `[trust.ots]` | `enabled`, `calendar_urls` | OpenTimestamps stamping |
| `[melt]` | `backend` | Melt payout: `inherit`, `mock`, or `barkd` |

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
4. Connect a real Ark ASP — signet via barkd ([docs/signet-asp.md](docs/signet-asp.md))
   or Arkade ([docs/arkade-asp.md](docs/arkade-asp.md)); set
   `[trust] vtxo_verify_mode = "vpack"` on mainnet
5. Set `signatory.backend = "remote"` and run **cdk-signatory** on a separate host
6. Expose `/transparency/summary` publicly for third-party reconciliation

Never commit RPC passwords, ASP keys, or tunnel tokens to git.

## Roadmap

| Milestone | Status |
| --------- | ------ |
| NUT HTTP scaffold + mock ASP | Done |
| VTXO inventory + refresh scheduler | Done |
| SignatoryPolicy + PoL epochs + OTS | Done |
| V-PACK verify at insert | Done |
| Signet ASP via barkd (`BarkdArkClient`) | Done (signet) |
| Arkade ASP client (`ArkadeArkClient`) | Done |
| CDK BDHKE + remote signatory (`BlindSigner`) | Done |
| Exit claim automation (barkd wallet API) | Done |
| Signet melt payout via barkd lightning pay | Done (signet) |
| Mainnet ASP hardening + melt at scale | Planned |
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

GNU Affero General Public License v3.0 or later — see [LICENSE](LICENSE).

If you run a **modified** version of this software as a **public network
service** (e.g. a mint API), AGPL requires you to offer corresponding source
to users interacting with that service.

Regulatory disclosures: [docs/DISCLAIMER.md](docs/DISCLAIMER.md)

## Links

- [Cashu NUTs](https://github.com/cashubtc/nuts)
- [Cashu CDK](https://github.com/cashubtc/cdk)
- [Ark protocol](https://ark-protocol.org/)
- [Signet ASP setup](docs/signet-asp.md) · [Arkade ASP setup](docs/arkade-asp.md)
- [PoL specification](https://gist.github.com/victorandre957/4f497d385e1fd9a47898480903f56b3e)
