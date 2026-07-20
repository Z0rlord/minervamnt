# Signet ASP integration (Second / Bark)

Minerva Mint talks to a live Ark ASP on **Bitcoin signet** through a local
**[barkd](https://second.tech/docs/barkd)** daemon. The mint does not connect
to the ASP directly; barkd holds the operator wallet and exposes a REST API.

## Public signet endpoints (Second)

| Service | URL |
| ------- | --- |
| Ark ASP | `https://ark.signet.2nd.dev` |
| Esplora | `https://esplora.signet.2nd.dev` |
| Faucet | `https://signet.2nd.dev` |

## Architecture

```text
Minerva Mint ──HTTP──▶ barkd (localhost:3535) ──▶ ark.signet.2nd.dev
                              │
                              └──▶ signet bitcoind / esplora
```

Set `ark.backend = "barkd"` in config (or `ARK_BACKEND=barkd`). The
[`BarkdArkClient`](../src/ark_barkd.rs) implements `board_sats`, `refresh_vtxo`,
and `unilateral_exit` via barkd's REST API.

## 1. Install barkd

Follow [Second install docs](https://second.tech/docs/barkd/install). On macOS
you can build from source or use published binaries when available.

## 2. Start barkd

```bash
export BARKD_DATADIR=~/.bark-signet-melt
barkd --datadir "$BARKD_DATADIR" --host 127.0.0.1 --port 3535
```

Operator wallet for this session is **`~/.bark-signet-melt`** (funded). Ignore `~/.bark-signet-melt-fresh` unless deliberately testing an empty wallet.

On first start, barkd prints an auth token. Save it:

```bash
export BARKD_AUTH_TOKEN='<token from barkd datadir>'
```

Retrieve later:

```bash
cat "$BARKD_DATADIR/auth_token"
```

## 3. Create signet wallet in barkd

```bash
curl -s -X POST http://127.0.0.1:3535/api/v1/wallet/create \
  -H "Authorization: Bearer $BARKD_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{
    "ark_server": "https://ark.signet.2nd.dev",
    "chain_source": { "esplora": { "url": "https://esplora.signet.2nd.dev" } },
    "network": "signet",
    "mnemonic": "<12/24 words — store securely>"
  }'
```

Verify ASP connectivity:

```bash
curl -s http://127.0.0.1:3535/api/v1/wallet/connected \
  -H "Authorization: Bearer $BARKD_AUTH_TOKEN"
```

Fetch ASP pubkey for your config:

```bash
curl -s http://127.0.0.1:3535/api/v1/wallet/ark-info \
  -H "Authorization: Bearer $BARKD_AUTH_TOKEN" | jq .server_pubkey
```

## 4. Fund on-chain wallet

Get a boarding address and receive signet sats from the faucet:

```bash
curl -s http://127.0.0.1:3535/api/v1/onchain/addresses/next \
  -H "Authorization: Bearer $BARKD_AUTH_TOKEN"
```

Board manually (optional — Minerva boards automatically when liquidity is low):

```bash
curl -s -X POST http://127.0.0.1:3535/api/v1/boards/board-amount \
  -H "Authorization: Bearer $BARKD_AUTH_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"amount_sat": 100000}'
```

## 5. Run Minerva on signet

```bash
cp config.signet.toml.example config.signet.toml
cp .env.example .env
# Edit .env:
#   ARK_BACKEND=barkd
#   BARKD_AUTH_TOKEN=...
#   BARKD_URL=http://127.0.0.1:3535
#   ARK_SERVER_URL=https://ark.signet.2nd.dev
#   ARK_SERVER_PUBKEY=<from ark-info>
#   MINERVA_CONFIG=config.signet.toml

cargo run
```

Check health:

```bash
curl -s http://127.0.0.1:3338/health | jq
```

Expect `ark_connected: true` when barkd is wired to the signet ASP.

## Config reference

| Key / env | Purpose |
| --------- | ------- |
| `ark.backend` / `ARK_BACKEND` | `mock` (default) or `barkd` |
| `ark.barkd_url` / `BARKD_URL` | barkd REST base (default `http://127.0.0.1:3535`) |
| `BARKD_AUTH_TOKEN` | Bearer token from barkd datadir |
| `ark.server_url` / `ARK_SERVER_URL` | ASP URL (`https://ark.signet.2nd.dev`) |
| `ark.exit_claim_address` / `ARK_EXIT_CLAIM_ADDRESS` | On-chain sweep target for auto-claim |
| `ark.auto_claim_exits` / `ARK_AUTO_CLAIM_EXITS` | Poll + claim when exit becomes claimable |
| `ark.poll_timeout_secs` | Max wait for board/refresh/exit (default 600) |
| `signatory.backend` / `SIGNATORY_BACKEND` | `mock`, `remote`, or `local` |
| `signatory.url` / `SIGNATORY_URL` | cdk-signatory gRPC URL (`https://localhost:3340` with mTLS) |
| `signatory.tls_dir` / `SIGNATORY_TLS_DIR` | Directory with `ca.pem`, `client.pem`, `client.key` |
| `melt.backend` / `MELT_BACKEND` | `inherit` (default), `mock`, or `barkd` |

## Melt payout (Lightning)

When `melt.backend` resolves to `barkd` (or `arkade` with `wallet_url`), melt
quotes decode the BOLT11 invoice amount and call barkd's
`POST /api/v1/lightning/pay`. Fee reserve comes from
`GET /api/v1/fees/lightning/pay`. The mint polls wallet history for the payment
preimage when barkd does not return it immediately.

Ensure the barkd wallet holds enough Ark balance to cover melt volume plus fees.

## Blind signing (CDK signatory)

Token mint/swap signatures use [`BlindSigner`](../src/blind_signer.rs):

| `signatory.backend` | Use |
| ------------------- | --- |
| `mock` | Deterministic dev signatures (default in `config.signet.toml`) |
| `remote` | [cdk-signatory](https://github.com/cashubtc/cdk) gRPC — production |
| `local` | In-process dhke with `SIGNATORY_MINT_SECRET` — dev only |

Keep mint keys off the mint host in production (`signatory.backend = "remote"`).

### Remote signatory (signet / local dev)

1. **Install** `cdk-signatory` 0.17.1 (or let the start script install to `.local/bin`):

   ```bash
   cargo install cdk-signatory@0.17.1 --root .local
   ```

2. **Start** — generates mTLS certs (first run), launches gRPC on `:3340`, and
   bootstraps a sat keyset when the DB is empty:

   ```bash
   bash scripts/start-cdk-signatory-signet.sh
   ```

   Work dir: `data/cdk-signatory-signet/` (seed, sqlite, certs). Server cert
   must include SAN `DNS:localhost` and `IP:127.0.0.1` (the start script sets
   this via `-extfile`).

3. **Point the mint** at the signatory:

   ```bash
   export SIGNATORY_BACKEND=remote
   export SIGNATORY_URL=https://localhost:3340
   export SIGNATORY_TLS_DIR="$PWD/data/cdk-signatory-signet"
   export MINERVA_CONFIG=config.signet.toml
   cargo run
   ```

   With mTLS, use `https://localhost:3340` (not bare `127.0.0.1` unless the
   server cert lists that IP in SAN). The mint normalizes `http://` → `https://`
   when `SIGNATORY_TLS_DIR` is set.

4. **Verify gRPC** (no mint required):

   ```bash
   bash scripts/signet-signatory-ping.sh
   ```

### Smoke tests

| Script | What it checks |
| ------ | -------------- |
| `scripts/signet-melt-smoke.sh` | Ark board/mint/melt path — **always uses mock signatory** |
| `scripts/signet-signatory-ping.sh` | Remote cdk-signatory gRPC + mTLS + active keyset |
| `scripts/signet-wallet-e2e.sh` | Real BDHKE mint→melt against remote signatory (`examples/wallet_e2e.rs`) |

Run both for a full signet dev check:

```bash
bash scripts/signet-signatory-ping.sh
BARKD_DATADIR="$HOME/.bark-signet-melt" MINERVA_CONFIG=config.signet.toml \
  bash scripts/signet-melt-smoke.sh
```

## Exit claim automation

When `ark.auto_claim_exits = true` and `ark.exit_claim_address` is set, Minerva
polls barkd's exit progress and calls `POST /exits/claim/vtxos` when outputs
become claimable. The `/ark/exit` response includes `phase` and `claim_txid`.

## Limitations (current)

- **`/v1/info` pubkey** — served from signatory keyset cache at startup; restart mint after signatory key rotation.
- **Melt backing** — pass `token_ids` (mint quote UUIDs) on `POST /v1/melt/bolt11`, or enable `release_backing_on_melt_fifo` in config.
- **Melt smoke + remote signatory** — `signet-melt-smoke.sh` uses mock signing; use `signet-signatory-ping.sh` for CDK gRPC. Full blind-sign e2e: `bash scripts/signet-wallet-e2e.sh` (`examples/wallet_e2e.rs` — valid BDHKE `B_` via cdk `dhke`).
- **Arkade on signet** — use Second/barkd here; see [arkade-asp.md](arkade-asp.md) for Arkade mainnet.

## Troubleshooting

| Symptom | Check |
| ------- | ----- |
| `ark_connected: false` | `wallet/connected`, ASP reachability, wallet created |
| Board timeout | On-chain funds? `boards/pending`, esplora sync |
| Refresh timeout | `wallet/rounds`, signet round interval (~minutes) |
| 401 from barkd | `BARKD_AUTH_TOKEN` matches datadir |
| `signatory connect: transport error` | Signatory running? `bash scripts/start-cdk-signatory-signet.sh`; use `https://localhost:3340` + `SIGNATORY_TLS_DIR` |
| `invalid B_: malformed public key` | Real signatory rejects random points — use a proper Cashu wallet or mock signatory for harness tests |

## Links

- [Barkd docs](https://second.tech/docs/barkd)
- [Signet getting started](https://second.tech/docs/bark/getting-started/signet)
- [Ark protocol](https://ark-protocol.org/)
