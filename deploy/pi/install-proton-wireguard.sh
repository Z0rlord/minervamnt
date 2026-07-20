#!/usr/bin/env bash
# Install WireGuard + Proton VPN tunnel on Pi (Ubuntu ARM64).
# Does NOT store secrets in git — expects a Proton-exported .conf on the host.
#
# On Pi:
#   PROTON_WG_CONF=/path/to/proton.conf bash deploy/pi/install-proton-wireguard.sh
#
# From Mac (copies conf then runs this):
#   PROTON_WG_CONF=~/Downloads/proton-jp.conf bash deploy/pi/push-proton-wg-from-mac.sh
#
# Generate conf: https://account.protonvpn.com → Downloads → WireGuard configuration
# Recommended: Platform=Linux, name short (e.g. "raspi"), server near you (JP/SG/HK).
set -euo pipefail

IFACE="${PROTON_WG_IFACE:-proton}"
CONF_DST="/etc/wireguard/${IFACE}.conf"
CONF_SRC="${PROTON_WG_CONF:-}"
ENABLE="${PROTON_WG_ENABLE:-1}"

log() { printf '[proton-wg] %s\n' "$*"; }

if [[ "${EUID:-$(id -u)}" -eq 0 ]]; then
  echo "Run as a normal user with sudo, not root." >&2
  exit 1
fi

need_cmd() { command -v "$1" >/dev/null 2>&1 || { echo "missing: $1" >&2; exit 1; }; }
need_cmd sudo

log "Installing wireguard packages"
sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq wireguard wireguard-tools

# Optional: helps wg-quick apply DNS= from Proton configs
if ! dpkg -s resolvconf >/dev/null 2>&1; then
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq resolvconf 2>/dev/null || true
fi

sudo mkdir -p /etc/wireguard
sudo chmod 700 /etc/wireguard

# Persist forwarding (needed later for Tailscale exit / LAN gateway)
echo 'net.ipv4.ip_forward=1' | sudo tee /etc/sysctl.d/99-minerva-forward.conf >/dev/null
echo 'net.ipv6.conf.all.forwarding=1' | sudo tee -a /etc/sysctl.d/99-minerva-forward.conf >/dev/null
sudo sysctl --system >/dev/null 2>&1 || sudo sysctl -p /etc/sysctl.d/99-minerva-forward.conf >/dev/null

if [[ -z "$CONF_SRC" ]]; then
  if [[ -f "/tmp/${IFACE}.conf" ]]; then
    CONF_SRC="/tmp/${IFACE}.conf"
  elif [[ -f "$HOME/${IFACE}.conf" ]]; then
    CONF_SRC="$HOME/${IFACE}.conf"
  fi
fi

if [[ -z "$CONF_SRC" || ! -f "$CONF_SRC" ]]; then
  cat >&2 <<EOF
Missing Proton WireGuard config.

1. Sign in: https://account.protonvpn.com
2. Downloads → WireGuard configuration
3. Create (Platform=Linux, short name e.g. raspi), Download .conf
4. Re-run with:
     PROTON_WG_CONF=/path/to/file.conf bash deploy/pi/install-proton-wireguard.sh
   or from Mac:
     PROTON_WG_CONF=~/Downloads/raspi.conf bash deploy/pi/push-proton-wg-from-mac.sh

WireGuard tools are installed; tunnel not started yet.
EOF
  exit 2
fi

log "Installing config -> $CONF_DST (mode 600)"
# Strip CRLF if downloaded on Windows/Mac Finder
TMP="$(mktemp)"
tr -d '\r' <"$CONF_SRC" >"$TMP"

# Ensure interface name stays short for wg-quick
if ! grep -q '^\[Interface\]' "$TMP"; then
  echo "Not a WireGuard config (missing [Interface]): $CONF_SRC" >&2
  rm -f "$TMP"
  exit 1
fi

# Keep Tailscale CGNAT + local link-local reachable if AllowedIPs is full-tunnel.
# Append once; idempotent.
if grep -qE '^AllowedIPs\s*=\s*0\.0\.0\.0/0' "$TMP" && ! grep -q 'PostUp = .*tailscale0' "$TMP"; then
  log "Patching config for Tailscale coexistence (exclude 100.64.0.0/10)"
  cat >>"$TMP" <<'EOF'

# Minerva: keep Tailscale mesh off the Proton default route when possible
PostUp = ip route add 100.64.0.0/10 dev tailscale0 table main 2>/dev/null || true
PostDown = ip route del 100.64.0.0/10 dev tailscale0 table main 2>/dev/null || true
EOF
fi

sudo cp "$TMP" "$CONF_DST"
rm -f "$TMP"
sudo chmod 600 "$CONF_DST"
sudo chown root:root "$CONF_DST"

# Drop the staging copy if it lived in /tmp or $HOME (avoid leftover private keys)
if [[ "$CONF_SRC" == /tmp/* || "$CONF_SRC" == "$HOME"/* ]]; then
  shred -u "$CONF_SRC" 2>/dev/null || rm -f "$CONF_SRC"
fi

UNIT="wg-quick@${IFACE}"
if [[ "$ENABLE" == "1" ]]; then
  log "Enabling $UNIT"
  sudo systemctl enable --now "$UNIT"
  sleep 2
  sudo systemctl --no-pager --full status "$UNIT" | head -20 || true
  log "WireGuard peers:"
  sudo wg show || true
  log "Public egress IP (expect Proton):"
  curl -4 -fsS --max-time 8 https://ifconfig.me || curl -4 -fsS --max-time 8 https://api.ipify.org || true
  echo
else
  log "Config installed; start later with: sudo systemctl enable --now $UNIT"
fi

log "Done. Tailscale check: tailscale status | head"
tailscale status 2>/dev/null | head -8 || true
