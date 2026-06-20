# Signet operator runbook

Production-shaped signet deployment for Minerva Mint with **barkd**, **CDK signatory**, and live melt.

## Architecture

```text
Wallet ──▶ Minerva Mint (:3338)
              ├── CDK signatory (:3340) — blind signatures + keysets
              └── barkd operator (:3535) ──▶ ark.signet.2nd.dev
Melt recv test wallet ── barkd (:3536) — cross-wallet LN invoices only
```

## 1. Operator wallet (barkd)

```bash
export BARKD_DATADIR=~/.bark-signet-melt
bark create --signet \
  --ark https://ark.signet.2nd.dev \
  --esplora https://esplora.signet.2nd.dev

barkd --datadir "$BARKD_DATADIR" --host 127.0.0.1 --port 3535
```

Fund via https://signet.2nd.dev using `bark address` (`tark1…`).

Confirm before minting:

```bash
curl -s -H "Authorization: Bearer $(<"$BARKD_DATADIR/auth_token")" \
  http://127.0.0.1:3535/api/v1/wallet/balance
# spendable_sat ≥ 50000, pending_lightning_send_sat = 0
```

## 2. Receive wallet (melt smoke / testing)

Use a **separate** datadir — never self-pay on the operator wallet.

```bash
export BARK_RECV_DATADIR=./data/bark-smoke-recv
bark create --signet --ark https://ark.signet.2nd.dev \
  --esplora https://esplora.signet.2nd.dev

barkd --datadir "$BARK_RECV_DATADIR" --host 127.0.0.1 --port 3536
```

## 3. CDK signatory (remote)

Run [cdk-signatory](https://github.com/cashubtc/cdk) separately with `mint_management_rpc.enabled = false`.

```toml
[signatory]
backend = "remote"
url = "http://127.0.0.1:3340"
```

Doppler / env: `SIGNATORY_BACKEND`, `SIGNATORY_URL`, `BARKD_AUTH_TOKEN`, `BARKD_URL`.

## 4. Minerva config

```bash
cp config.signet.toml.example config.signet.toml
export MINERVA_CONFIG=config.signet.toml
doppler run -- cargo run --release
```

Verify:

```bash
curl -s http://127.0.0.1:3338/v1/info | jq '.pubkey, .nuts."4"'
curl -s http://127.0.0.1:3338/v1/keysets
curl -s http://127.0.0.1:3338/health | jq
```

## 5. Smoke test

```bash
BARKD_DATADIR=~/.bark-signet-melt bash scripts/signet-melt-smoke.sh
```

Expect **PASS** on health, mint, melt quote, and melt pay.

## 6. Melt backing release

When melting, pass mint quote UUIDs so VTXO backing is released:

```json
{
  "quote": "<melt-quote-id>",
  "inputs": [...],
  "token_ids": ["<mint-quote-uuid>"]
}
```

Or enable FIFO release in config:

```toml
[trust]
release_backing_on_melt = true
release_backing_on_melt_fifo = true
```

## Troubleshooting

| Symptom | Fix |
| ------- | --- |
| `pending_lightning_send_sat` high | Wait for Ark round sync; avoid restarting barkd mid-melt; use fresh wallet if stuck |
| Melt timeout | Ensure recv barkd on :3536 is running during melt |
| Wrong keyset on mint | Check `/v1/keysets`; align CDK signatory active id |
| `ark_connected: false` | barkd running? `wallet/connected` → 200? |

## Do not

- Commit `config.signet.toml`, mnemonics, or `auth_token` files
- Run melt against a wedged wallet (96k+ pending LN)
- Use operator wallet for melt invoices (self-pay fails)
