#!/usr/bin/env bash
# Copy Pi deploy scripts over SSH and run full node stack setup.
set -euo pipefail

PI_HOST="${PI_HOST:-}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/raspi_key}"
SSH_USER="${SSH_USER:-ubuntu}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

log() { printf '[from-mac] %s\n' "$*"; }

if [[ -z "$PI_HOST" ]]; then
  for candidate in 192.168.2.2 100.75.188.125 172.16.2.14; do
    if ssh -i "$SSH_KEY" -o BatchMode=yes -o ConnectTimeout=3 -o StrictHostKeyChecking=no \
      "$SSH_USER@$candidate" true 2>/dev/null; then
      PI_HOST="$candidate"
      break
    fi
  done
fi

[[ -n "$PI_HOST" ]] || {
  log "Pi not reachable. Try:"
  log "  bash deploy/pi/connect-ethernet-mac.sh"
  log "  export PI_HOST=ubuntu@100.75.188.125 && bash $0"
  exit 1
}

log "Using $SSH_USER@$PI_HOST"
ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$SSH_USER@$PI_HOST" 'mkdir -p ~/minervamnt/deploy/pi'
rsync -av -e "ssh -i $SSH_KEY -o StrictHostKeyChecking=no" \
  "$REPO_ROOT/deploy/pi/" "$SSH_USER@$PI_HOST:~/minervamnt/deploy/pi/"
ssh -i "$SSH_KEY" -o StrictHostKeyChecking=no "$SSH_USER@$PI_HOST" \
  'chmod +x ~/minervamnt/deploy/pi/*.sh && REPO_DIR=~/minervamnt bash ~/minervamnt/deploy/pi/setup-node-stack.sh'
