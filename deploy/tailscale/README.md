# Run Minerva Mint on a VPS / Pi over Tailscale

Expose the mint **privately on your tailnet** instead of via Cloudflare Tunnel.
The mint binds to the host's Tailscale IP (or `127.0.0.1`) and a firewall rule
keeps `:3338` off the LAN and public internet. Optionally, `tailscale serve`
adds HTTPS with a MagicDNS certificate.

This is reference material — adapt users, paths, and network layout. It is not a
turnkey installer.

## 1. Install + connect Tailscale on the host

```bash
curl -fsSL https://tailscale.com/install.sh | sh
sudo tailscale up            # approve the login URL in a browser
tailscale ip -4              # note the 100.x.y.z tailnet IP
```

## 2. Install the mint (Tailscale-only)

Stage a release binary at `/opt/minervamnt/bin/minerva-mint` (build with
`cargo build --release` on a matching arch), or build in-place with
`BUILD_FROM_SRC=1`:

```bash
# From a checkout on the host
BUILD_FROM_SRC=1 bash deploy/tailscale/install-mint-tailscale.sh
```

The script:

- derives the Tailscale IP and writes `.env` with `BIND_ADDR=<ts-ip>:3338`
  (the mint never listens on `0.0.0.0`);
- installs + enables the `minerva-mint` systemd unit;
- adds a UFW rule allowing `:3338` **only on `tailscale0`**;
- waits for `/health` and prints `/v1/info`.

Reach the mint from any tailnet device:

```bash
curl -s http://100.x.y.z:3338/health | jq
# or via MagicDNS:
curl -s http://<hostname>.<tailnet>.ts.net:3338/health | jq
```

## 3. Optional: HTTPS via `tailscale serve`

For a TLS endpoint (MagicDNS cert) without a reverse proxy:

```bash
bash deploy/tailscale/serve-https.sh
# -> https://<hostname>.<tailnet>.ts.net/v1/info  (tailnet-only)
```

`ENABLE_FUNNEL=1 bash deploy/tailscale/serve-https.sh` publishes it to the public
internet via Tailscale Funnel — only do this deliberately for a public mint.

## Manual equivalent (no script)

```bash
# .env on the host
BIND_ADDR=100.x.y.z:3338
MINERVA_MINT_URL=http://100.x.y.z:3338

sudo ufw allow in on tailscale0 to any port 3338 proto tcp
sudo systemctl enable --now minerva-mint
```

## Notes

- Bind to the Tailscale IP (not `0.0.0.0`) so the API is only reachable over the
  tailnet, even if UFW is disabled.
- The mint auto-creates its SQLite data dir (`data/` from `config.toml`); no
  manual `mkdir` is required.
- Point `[bitcoin].rpc_url` at a node reachable over your tailnet (e.g. another
  tailnet host) for `/health` chain data — the mock ASP dev mode does not need it.
- Keep RPC/ASP creds in the host `.env` (mode `600`), never in git.
