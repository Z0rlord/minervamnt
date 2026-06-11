# Raspberry Pi 5 (pi5) — applied configuration reference

This documents what was configured on **pi5**. Idempotent automation lives in `harden.sh` and `install-bitcoind.sh` — safe to re-run for drift correction. **Do not re-run destructive steps** (disk formatting, full re-sync) unless you intend to rebuild the node.

## Host facts

| Item | Value |
|------|-------|
| Hostname | `pi5` |
| OS | Ubuntu (ARM64) |
| Tailscale IP | `100.75.188.125` |
| SSH | `ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125` |

## Bitcoin Core 31.0

- **Role:** Full node with transaction index (`txindex=1`)
- **Data directory:** `/mnt/btcdata/bitcoin`
- **RPC bind:** Tailscale interface only (`100.75.188.125:8332`)
- **RPC user:** `minerva`
- **RPC password:** stored on Pi at `/etc/bitcoin/rpc-credentials` (root-only, `chmod 600`)
- **Firewall:** UFW allows TCP `8332` on `tailscale0` only

### Verify sync status (from any Tailscale peer)

```bash
# Copy password from Pi (do not commit):
# ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125 'sudo cat /etc/bitcoin/rpc-credentials'

curl -s --user minerva:<password> \
  --data-binary '{"jsonrpc":"1.0","id":"sync","method":"getblockchaininfo","params":[]}' \
  -H 'content-type: text/plain;' \
  http://100.75.188.125:8332 | jq '.result | {chain, blocks, headers, verificationprogress, initialblockdownload}'
```

**Note:** Initial block download (IBD) takes **days** on a Pi. The mint can start before sync completes, but Bitcoin-dependent features need a synced node.

### bitcoin.conf highlights (reference)

```ini
server=1
txindex=1
datadir=/mnt/btcdata/bitcoin
rpcbind=100.75.188.125
rpcallowip=100.64.0.0/10
rpcuser=minerva
# rpcpassword in /etc/bitcoin/rpc-credentials (included via includeconf)
```

## cloudflared

- Binary installed on pi5
- Tunnel for **minervamnt.xyz** requires a one-time Cloudflare login (see repo `README.md` and `deploy/cloudflared/config.yml.example`)
- Ingress target: `http://localhost:3338` (Minerva Mint HTTP server)

## Minerva Mint service

- Systemd unit template: `deploy/systemd/minerva-mint.service`
- Expected install path: `/opt/minervamnt`
- Environment: `/opt/minervamnt/.env` (copy from `.env.example`, set `BITCOIN_RPC_PASSWORD` from Pi credentials file)

## Security notes

- RPC is **not** exposed to the public internet — Tailscale + UFW only
- Never commit RPC passwords or Cloudflare tunnel credentials to git
- Rotate RPC password if credentials may have leaked
