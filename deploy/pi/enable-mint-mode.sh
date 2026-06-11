#!/usr/bin/env bash
# Re-enable mint API and restore cloudflared to port 3338.
# Run on Pi as ubuntu with sudo: bash deploy/pi/enable-mint-mode.sh

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-/opt/minervamnt}"
CLOUDFLARED_CONFIG="/etc/cloudflared/config.yml"

echo "==> Stopping landing page server"
sudo systemctl stop minervamnt-landing || true
sudo systemctl disable minervamnt-landing || true

echo "==> Restoring cloudflared ingress to port 3338"
sudo tee "${CLOUDFLARED_CONFIG}" > /dev/null <<'EOF'
tunnel: 4b000692-9937-4185-a25a-c0ffe192056a
credentials-file: /etc/cloudflared/4b000692-9937-4185-a25a-c0ffe192056a.json

ingress:
  - hostname: minervamnt.xyz
    service: http://localhost:3338
  - service: http_status:404
EOF
sudo systemctl restart cloudflared

echo "==> Starting minerva-mint"
sudo systemctl enable --now minerva-mint

echo "==> Verifying"
sleep 2
systemctl is-active minerva-mint
curl -sS http://127.0.0.1:3338/health | head -3
echo "Done. Test: curl -s https://minervamnt.xyz/health"
