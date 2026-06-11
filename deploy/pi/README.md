# Edge node deployment (optional reference)

Example notes for running Minerva Mint on a small Linux host (ARM SBC, VPS, or
home server). **Adapt paths, users, and network layout to your environment.**

This is not a turnkey installer — review each script before running on production
hardware.

## Suggested layout

| Component | Typical role |
| --------- | ------------ |
| **Mint host** | Runs `minerva-mint` binary, SQLite data dir |
| **Bitcoin Core** | Same host or reachable via private RPC |
| **Reverse proxy / tunnel** | TLS termination (nginx, Caddy, Cloudflare Tunnel, etc.) |

## Environment variables for helper scripts

Scripts under `deploy/pi/` that SSH to a remote host expect:

```bash
export DEPLOY_HOST="user@your-mint-host.example"   # SSH target
export DEPLOY_SSH_KEY="${HOME}/.ssh/your_key"      # optional; default ssh-agent
export MINT_DOMAIN="mint.example.com"              # public hostname
```

Example:

```bash
export DEPLOY_HOST="minerva@203.0.113.10"
bash deploy/pi/deploy-landing-from-mac.sh
```

## Bitcoin Core

`install-bitcoind.sh` is a **reference** for installing Bitcoin Core with RPC
bound to a private interface (example uses a VPN interface name). Edit
`rpcallowip`, datadir, and systemd unit paths before use.

Store RPC credentials outside git:

```bash
# On the node — root-only file, mode 600
BITCOIN_RPC_USER=bitcoinrpc
BITCOIN_RPC_PASSWORD=<generated>
```

Copy values into your mint host `.env` (see `.env.example`).

## systemd

Example units live in `deploy/systemd/`:

```bash
sudo cp deploy/systemd/minerva-mint.service /etc/systemd/system/
# Edit User=, WorkingDirectory=, EnvironmentFile= for your layout
sudo systemctl daemon-reload
sudo systemctl enable --now minerva-mint
```

## Landing-only mode

To serve static HTML without the mint API:

```bash
bash deploy/pi/enable-landing-mode.sh    # on the host
bash deploy/pi/enable-mint-mode.sh       # restore mint API
```

## Recovery scripts

`recover-boot-drive-mac.sh` and `run-recover-noninteractive.sh` are **operator
maintenance utilities** for SD/USB recovery workflows. Before use:

1. Replace placeholder SSH public keys in the cloud-init snippet with your own.
2. Remove or edit any host-specific paths in the script output.
3. Do not run destructive disk operations without backups.

## Security checklist

- [ ] RPC not exposed to the public internet
- [ ] Firewall allows admin access only from trusted networks
- [ ] `.env` permissions `600`, owned by the service user
- [ ] Separate signatory host in production (see [trust model](../../docs/trust-model.md))
