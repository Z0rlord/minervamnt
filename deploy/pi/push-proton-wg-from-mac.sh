#!/usr/bin/env bash
# Copy a Proton WireGuard .conf to raspi-sd and run install-proton-wireguard.sh.
#
#   PROTON_WG_CONF=~/Downloads/raspi.conf bash deploy/pi/push-proton-wg-from-mac.sh
#
# Env:
#   PI_HOST       default: raspi-sd (Tailscale MagicDNS) or set 100.x
#   PI_USER       default: z0rlord
#   SSH_KEY       default: ~/.ssh/raspi_key
#   PROTON_WG_CONF  path to downloaded .conf (required)
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
PI_HOST="${PI_HOST:-raspi-sd}"
PI_USER="${PI_USER:-z0rlord}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/raspi_key}"
CONF="${PROTON_WG_CONF:?Set PROTON_WG_CONF=/path/to/proton.conf}"

log() { printf '[push-proton-wg] %s\n' "$*"; }

[[ -f "$CONF" ]] || { echo "missing conf: $CONF" >&2; exit 1; }
[[ -f "$SSH_KEY" ]] || { echo "missing SSH key: $SSH_KEY" >&2; exit 1; }

SSH=(ssh -i "$SSH_KEY" -o BatchMode=yes -o ConnectTimeout=15)
SCP=(scp -i "$SSH_KEY" -o BatchMode=yes -o ConnectTimeout=15)
TARGET="${PI_USER}@${PI_HOST}"

log "Copying install script + conf -> $TARGET"
"${SCP[@]}" \
  "$ROOT/deploy/pi/install-proton-wireguard.sh" \
  "$CONF" \
  "${TARGET}:/tmp/"

REMOTE_CONF="/tmp/$(basename "$CONF")"
log "Running installer on Pi"
"${SSH[@]}" "$TARGET" \
  "chmod +x /tmp/install-proton-wireguard.sh && PROTON_WG_CONF='$REMOTE_CONF' bash /tmp/install-proton-wireguard.sh"

log "OK — verify: ssh $TARGET 'curl -4 -fsS https://ifconfig.me; echo; sudo wg show'"
