#!/usr/bin/env bash
# Deploy landing mode to a remote host over SSH.
# Requires: DEPLOY_HOST (e.g. user@203.0.113.10)
# Optional: DEPLOY_SSH_KEY, REMOTE_APP_DIR (default /opt/minervamnt)

set -euo pipefail

: "${DEPLOY_HOST:?Set DEPLOY_HOST=user@your-host}"

PI="$DEPLOY_HOST"
KEY="${DEPLOY_SSH_KEY:-}"
REMOTE_DIR="${REMOTE_APP_DIR:-/opt/minervamnt}"
REPO="$(cd "$(dirname "$0")/../.." && pwd)"

SSH_OPTS=(-o StrictHostKeyChecking=accept-new)
if [[ -n "$KEY" ]]; then
  SSH_OPTS+=(-i "$KEY")
fi

echo "==> Copying files to ${PI}"
ssh "${SSH_OPTS[@]}" "$PI" "sudo mkdir -p ${REMOTE_DIR}/landing && sudo chown \$(whoami):\$(whoami) ${REMOTE_DIR}/landing"
scp "${SSH_OPTS[@]}" "$REPO/landing/index.html" "$PI:${REMOTE_DIR}/landing/index.html"
scp "${SSH_OPTS[@]}" "$REPO/deploy/systemd/minervamnt-landing.service" "$PI:/tmp/minervamnt-landing.service"
scp "${SSH_OPTS[@]}" "$REPO/deploy/cloudflared/config-landing.yml.example" "$PI:/tmp/cloudflared-landing.yml"
scp "${SSH_OPTS[@]}" "$REPO/deploy/pi/enable-landing-mode.sh" "$PI:/tmp/enable-landing-mode.sh"

echo "==> Enabling landing mode on remote host"
ssh "${SSH_OPTS[@]}" "$PI" 'bash /tmp/enable-landing-mode.sh'

if [[ -n "${MINT_DOMAIN:-}" ]]; then
  echo "==> Public test (${MINT_DOMAIN})"
  curl -sS "https://${MINT_DOMAIN}/" | head -10 || true
fi
