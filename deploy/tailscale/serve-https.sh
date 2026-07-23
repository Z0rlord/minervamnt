#!/usr/bin/env bash
# Expose the local mint over HTTPS on your tailnet via `tailscale serve`.
#
# `tailscale serve` terminates TLS with a MagicDNS cert and proxies to the local
# mint. The service stays private to your tailnet (no public exposure) unless you
# explicitly enable Funnel.
#
# Prereqs:
#   - Tailscale connected, MagicDNS + HTTPS certificates enabled for the tailnet
#     (https://login.tailscale.com/admin/dns).
#   - Mint running locally on 127.0.0.1:$MINT_PORT (or on the tailnet IP).
#
# Usage:
#   bash deploy/tailscale/serve-https.sh          # tailnet-only HTTPS on :443
#   ENABLE_FUNNEL=1 bash deploy/tailscale/serve-https.sh   # public via Funnel (opt-in)
set -euo pipefail

MINT_PORT="${MINT_PORT:-3338}"
ENABLE_FUNNEL="${ENABLE_FUNNEL:-0}"

command -v tailscale >/dev/null 2>&1 || { echo "tailscale not installed" >&2; exit 1; }

FQDN="$(tailscale status --json 2>/dev/null | jq -r '.Self.DNSName' | sed 's/\.$//' || true)"

if [[ "$ENABLE_FUNNEL" == "1" ]]; then
  echo "[serve] enabling PUBLIC Funnel -> 127.0.0.1:${MINT_PORT}"
  sudo tailscale funnel --bg "http://127.0.0.1:${MINT_PORT}"
else
  echo "[serve] enabling tailnet-only HTTPS -> 127.0.0.1:${MINT_PORT}"
  sudo tailscale serve --bg "http://127.0.0.1:${MINT_PORT}"
fi

echo "[serve] current config:"
tailscale serve status || true
[[ -n "$FQDN" ]] && echo "[serve] mint reachable at: https://${FQDN}/v1/info"
