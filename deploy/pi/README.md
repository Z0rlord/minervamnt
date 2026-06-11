# Raspberry Pi 5 (pi5) — applied configuration

Reference for what is already configured on **pi5**. Do **not** re-run destructive steps (disk wipe, full re-sync, UFW lockout) without a deliberate plan.

## Host

| Item | Value |
|------|-------|
| Hostname | `pi5` |
| OS user | `ubuntu` |
| Tailscale IP | `100.75.188.125` |
| SSH | `ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125` |

## Bitcoin Core 31.0

- **datadir:** `/mnt/btcdata/bitcoin`
- **Mode:** full node with `txindex=1`
- **RPC:** `http://100.75.188.125:8332` (Tailscale only)
- **RPC user:** `minerva`
- **RPC password:** stored on Pi at `/etc/bitcoin/rpc-credentials` (root-only, `600`)
- **UFW:** port `8332` allowed on `tailscale0` only

Copy the password to your mint host `.env` (never commit it):

```bash
ssh -i ~/.ssh/raspi_key ubuntu@100.75.188.125 \
  'sudo cat /etc/bitcoin/rpc-credentials'
```

### Sync status

Initial block download (IBD) takes **days**. Check progress:

```bash
bitcoin-cli -rpcconnect=100.75.188.125 -rpcuser=minerva -rpcpassword='<password>' \
  getblockchaininfo | jq '{blocks, headers, verificationprogress, initialblockdownload}'
```

Or from any Tailscale peer with curl:

```bash
curl -s --user minerva:'<password>' \
  --data-binary '{"jsonrpc":"1.0","id":"sync","method":"getblockchaininfo","params":[]}' \
  -H 'content-type: text/plain;' \
  http://100.75.188.125:8332/ | jq '.result | {blocks, headers, verificationprogress, initialblockdownload}'
```

## cloudflared

- Binary installed on pi5
- Tunnel for **minervamnt.xyz** still needs a one-time Cloudflare login (see root `README.md` and `deploy/cloudflared/config.yml.example`)

## Minerva Mint service

After building the binary on pi5, install the systemd unit:

```bash
sudo cp deploy/systemd/minerva-mint.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now minerva-mint
```

Ensure `.env` on the Pi has `BITCOIN_RPC_PASSWORD` from `/etc/bitcoin/rpc-credentials`.

## Landing page mode (mint disabled)

To serve a static page at minervamnt.xyz without the mint API:

```bash
cd /opt/minervamnt && git pull
bash deploy/pi/enable-landing-mode.sh
```

Re-enable mint API:

```bash
bash deploy/pi/enable-mint-mode.sh
```
