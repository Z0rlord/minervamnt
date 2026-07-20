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

## Pi 5 node stack (Bitcoin + Alby Hub)

| Script | Where to run | Purpose |
| ------ | ------------ | ------- |
| `connect-ethernet-mac.sh` | Mac | USB Ethernet link to Pi (`192.168.2.1` ↔ `192.168.2.2`) |
| `patch-boot-vpn.sh` | Mac (SD mounted) | Tailscale + ZeroTier on first boot (browser/dashboard auth) |
| `patch-boot-ethernet-access.sh` | Mac (SD mounted) | Boot partition WiFi + eth static IP |
| `setup-wifi.sh` | Pi | Connect to `SST-WiFi` via nmcli/netplan |
| `install-bitcoind.sh` | Pi | Full node on SSD at `/mnt/btcdata` |
| `install-albyhub.sh` | Pi | Alby Hub LDK wired to local `bitcoind` RPC |
| `setup-node-stack.sh` | Pi | All of the above in order |
| `run-stack-from-mac.sh` | Mac | Rsync scripts + run stack over SSH |
| `install-proton-wireguard.sh` | Pi | WireGuard + Proton VPN tunnel (`wg-quick@proton`) |
| `push-proton-wg-from-mac.sh` | Mac | SCP Proton `.conf` + run installer over Tailscale |

### Proton VPN (WireGuard)

Official Proton GUI/CLI is awkward on headless ARM; use a WireGuard config instead:

1. [account.protonvpn.com](https://account.protonvpn.com) → **Downloads** → **WireGuard configuration**
2. Platform **Linux**, short name (e.g. `raspi`), pick a nearby server
3. Download the `.conf`, then:

```bash
export PI_HOST=raspi-sd   # or Tailscale IP
PROTON_WG_CONF=~/Downloads/raspi.conf bash deploy/pi/push-proton-wg-from-mac.sh
```

No Proton private keys belong in git. The installer shreds the staging copy on the Pi after install.

```bash
# Ethernet direct to Mac (USB adapter)
bash deploy/pi/connect-ethernet-mac.sh

# Or once Pi is on Tailscale
export PI_HOST=100.75.188.125
bash deploy/pi/run-stack-from-mac.sh
```

Alby Hub UI: `http://pi5.local:8080` — complete the setup wizard after install.
Bitcoin keeps syncing in the background; Alby Hub uses `bitcoind` RPC once blocks are available.

### VPN on first boot (Tailscale + ZeroTier)

With the SD card mounted as `/Volumes/system-boot` (e.g. `disk4`):

```bash
# Tailscale only (ZeroTier join skipped)
BOOT_VOL=/Volumes/system-boot DISK=disk4 bash deploy/pi/patch-boot-vpn.sh

# Both — set your ZeroTier network ID
ZT_NETWORK_ID=your16charnetworkid BOOT_VOL=/Volumes/system-boot DISK=disk4 bash deploy/pi/patch-boot-vpn.sh
```

On first boot the Pi prints a **Tailscale login URL** (approve in browser). For ZeroTier, approve the node at [my.zerotier.com](https://my.zerotier.com). No auth keys are stored on the SD card.

## Security checklist

- [ ] RPC not exposed to the public internet
- [ ] Firewall allows admin access only from trusted networks
- [ ] `.env` permissions `600`, owned by the service user
- [ ] Separate signatory host in production (see [trust model](../../docs/trust-model.md))
