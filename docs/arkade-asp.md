# Arkade ASP integration

Minerva Mint can use an **[Arkade](https://arkade.computer)** Ark ASP via
`ark.backend = "arkade"`. ASP health checks use the `ark-rest` client against
`ark.server_url`; board, refresh, and exit flows use the same wallet HTTP API as
barkd (`WalletHttpClient` in [`ark_wallet_http.rs`](../src/ark_wallet_http.rs)).

## Architecture

```text
Minerva Mint ──HTTP──▶ wallet daemon (localhost) ──▶ Arkade ASP
         │                      │
         └── ark-rest ──────────┘ (get_info / connectivity)
```

## Configuration

```toml
[ark]
backend = "arkade"
server_url = "https://<your-arkade-asp>"
server_pubkey = "<hex>"
wallet_url = "http://127.0.0.1:3535"
auto_claim_exits = false
# exit_claim_address = "bc1..."
```

Environment overrides:

| Env | Purpose |
| --- | ------- |
| `ARK_BACKEND=arkade` | Select Arkade client |
| `ARK_SERVER_URL` | Arkade ASP REST base |
| `ARK_WALLET_URL` | Wallet daemon HTTP (barkd-compatible API) |
| `ARKADE_WALLET_AUTH_TOKEN` or `BARKD_AUTH_TOKEN` | Bearer auth for wallet daemon |
| `ARK_EXIT_CLAIM_ADDRESS` | Auto-claim sweep address |
| `ARK_AUTO_CLAIM_EXITS` | Enable exit claim polling |

## Wallet daemon

Arkade operators typically run a local wallet daemon exposing barkd-style REST
endpoints under `/api/v1/`. Point `ark.wallet_url` at that service and supply
the auth token from the daemon datadir.

`ArkadeArkClient::ping()` checks **both** ark-rest `get_info` and
`GET /api/v1/wallet/connected` when `wallet_url` is set.

Verify connectivity:

```bash
curl -s http://127.0.0.1:3535/api/v1/wallet/connected \
  -H "Authorization: Bearer $ARKADE_WALLET_AUTH_TOKEN"
```

## Mainnet operator checklist

1. Copy `config.mainnet.toml.example` → `config.mainnet.toml`
2. Set `ark.server_url`, `server_pubkey`, `wallet_url`
3. Run wallet daemon + fund ASP balance for melt volume
4. Run CDK signatory (`signatory.backend = "remote"`)
5. Set `trust.vtxo_verify_mode = "vpack"`
6. Enable melt backing release (`release_backing_on_melt` / `token_ids`)
7. Point Doppler: `ARK_BACKEND=arkade`, `ARK_WALLET_URL`, `ARKADE_WALLET_AUTH_TOKEN`

| Env | Purpose |
| --- | ------- |
| `MELT_BACKEND` | `inherit` (default) uses ark backend for live LN melt |
| `MINERVA_RELEASE_BACKING_ON_MELT` | Release VTXO mapping after melt |
| `MINERVA_VTXO_VERIFY_MODE` | `vpack` on mainnet |

## Signet vs mainnet

Second signet (`ark.signet.2nd.dev`) is documented in
[signet-asp.md](signet-asp.md) with `ark.backend = "barkd"`. Use this Arkade
path for Arkade-hosted ASPs on mainnet or test environments they provide.

## Related

- [`ArkadeArkClient`](../src/ark_arkade.rs)
- [`build_ark_client`](../src/ark_client.rs)
- [Ark protocol](https://ark-protocol.org/)
