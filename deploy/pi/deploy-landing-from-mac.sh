#!/usr/bin/env bash
# Deploy landing mode to pi5 from your Mac (requires SSH).
# Usage: bash deploy/pi/deploy-landing-from-mac.sh

set -euo pipefail

PI="ubuntu@100.75.188.125"
KEY="${HOME}/.ssh/raspi_key"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"

SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new)

echo "==> Copying files to pi5"
ssh "${SSH_OPTS[@]}" "$PI" 'sudo mkdir -p /opt/minervamnt/landing && sudo chown ubuntu:ubuntu /opt/minervamnt/landing'
scp "${SSH_OPTS[@]}" "$REPO/landing/index.html" "$PI:/opt/minervamnt/landing/index.html"
scp "${SSH_OPTS[@]}" "$REPO/deploy/systemd/minervamnt-landing.service" "$PI:/tmp/minervamnt-landing.service"
scp "${SSH_OPTS[@]}" "$REPO/deploy/cloudflared/config-landing.yml.example" "$PI:/tmp/cloudflared-landing.yml"
scp "${SSH_OPTS[@]}" "$REPO/deploy/pi/enable-landing-mode.sh" "$PI:/tmp/enable-landing-mode.sh"

echo "==> Enabling landing mode on pi5"
ssh "${SSH_OPTS[@]}" "$PI" 'bash /tmp/enable-landing-mode.sh'

echo "==> Public test"
curl -sS https://minervamnt.xyz/ | head -10
echo ""
curl -sS -o /dev/null -w "/health => HTTP %{http_code}\n" https://minervamnt.xyz/health
curl -sS -o /dev/null -w "/v1/info => HTTP %{http_code}\n" https://minervamnt.xyz/v1/info
