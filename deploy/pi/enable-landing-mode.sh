#!/usr/bin/env bash
# Switch minervamnt.xyz to static landing page (disable mint API).
# Run on Pi as ubuntu with sudo: bash deploy/pi/enable-landing-mode.sh

set -euo pipefail

REPO_ROOT="${REPO_ROOT:-/opt/minervamnt}"
LANDING_DIR="${REPO_ROOT}/landing"
CLOUDFLARED_CONFIG="/etc/cloudflared/config.yml"

echo "==> Stopping and disabling minerva-mint"
sudo systemctl stop minerva-mint || true
sudo systemctl disable minerva-mint || true

echo "==> Ensuring landing directory exists"
sudo mkdir -p "${LANDING_DIR}"
sudo cp "${REPO_ROOT}/landing/index.html" "${LANDING_DIR}/index.html"
sudo chown -R ubuntu:ubuntu "${LANDING_DIR}"

echo "==> Installing minervamnt-landing systemd unit"
sudo cp "${REPO_ROOT}/deploy/systemd/minervamnt-landing.service" /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now minervamnt-landing

echo "==> Updating cloudflared ingress to port 8080"
sudo cp "${REPO_ROOT}/deploy/cloudflared/config-landing.yml.example" "${CLOUDFLARED_CONFIG}"
sudo systemctl restart cloudflared

echo "==> Verifying"
sleep 2
systemctl is-active minerva-mint && echo "WARN: minerva-mint still active" || echo "OK: minerva-mint inactive"
systemctl is-active minervamnt-landing
ss -tlnp | grep -E ':8080|:3338' || true
curl -sS http://127.0.0.1:8080/ | head -5
echo "Done. Test: curl -s https://minervamnt.xyz"
