#!/usr/bin/env bash
# Configure WiFi on Pi 5 (Ubuntu). Adds/merges SSID into netplan; keeps existing APs.
# Defaults: SST-WiFi. Optional: KEEP_SSID / KEEP_PASS for a second AP (e.g. Perseus).
# Set FORCE_CONNECT=1 only when the target SSID is known-visible (select_network disables others).
set -euo pipefail

WIFI_SSID="${WIFI_SSID:-SST-WiFi}"
WIFI_PASS="${WIFI_PASS:-3445SST2805}"
COUNTRY="${WIFI_COUNTRY:-JP}"
FORCE_CONNECT="${FORCE_CONNECT:-0}"

log() { printf '[wifi] %s\n' "$*"; }

CURRENT_SSID=""
if command -v wpa_cli >/dev/null 2>&1; then
  CURRENT_SSID=$(sudo wpa_cli -i wlan0 status 2>/dev/null | awk -F= '/^ssid=/{print $2; exit}' || true)
fi

if [[ -n "$CURRENT_SSID" && "$CURRENT_SSID" == "$WIFI_SSID" && "$FORCE_CONNECT" != "1" ]]; then
  log "already on $WIFI_SSID: $(ip -4 -o addr show wlan0 2>/dev/null | awk '{print $4}')"
  exit 0
fi

if command -v nmcli >/dev/null 2>&1; then
  log "nmcli: connecting to $WIFI_SSID"
  sudo nmcli device wifi rescan || true
  sudo nmcli device wifi connect "$WIFI_SSID" password "$WIFI_PASS" ifname wlan0
elif [[ -d /etc/netplan ]]; then
  log "netplan: ensuring $WIFI_SSID in /etc/netplan/99-minerva-wifi.yaml"
  EXTRA_YAML=""
  if [[ -n "${KEEP_SSID:-}" && -n "${KEEP_PASS:-}" ]]; then
    EXTRA_YAML=$(printf '        "%s":\n          password: "%s"\n' "$KEEP_SSID" "$KEEP_PASS")
  fi
  # shellcheck disable=SC2086
  sudo tee /etc/netplan/99-minerva-wifi.yaml >/dev/null <<EOF
network:
  version: 2
  wifis:
    wlan0:
      optional: false
      dhcp4: true
      regulatory-domain: ${COUNTRY}
      access-points:
        "${WIFI_SSID}":
          password: "${WIFI_PASS}"
${EXTRA_YAML}
EOF
  sudo chmod 600 /etc/netplan/99-minerva-wifi.yaml
  sudo netplan generate
  sudo netplan apply
  if command -v wpa_cli >/dev/null 2>&1; then
    sleep 2
    SST_ID=$(sudo wpa_cli -i wlan0 list_networks 2>/dev/null | awk -F'\t' -v s="$WIFI_SSID" '$2==s{print $1; exit}' || true)
    if [[ -n "${SST_ID:-}" ]]; then
      # Prefer target SSID but do not disable others unless FORCE_CONNECT=1
      sudo wpa_cli -i wlan0 set_network "$SST_ID" priority 20 >/dev/null || true
      sudo wpa_cli -i wlan0 enable_network "$SST_ID" >/dev/null || true
      if [[ "$FORCE_CONNECT" == "1" ]]; then
        sudo wpa_cli -i wlan0 select_network "$SST_ID" >/dev/null || true
      else
        sudo wpa_cli -i wlan0 reassociate >/dev/null || true
      fi
    fi
  fi
else
  echo "No nmcli or netplan found" >&2
  exit 1
fi

sleep 5
ip -br a
SSID_NOW=$(sudo wpa_cli -i wlan0 status 2>/dev/null | awk -F= '/^ssid=/{print $2; exit}' || true)
log "associated ssid=${SSID_NOW:-unknown}"
ping -c2 -W3 8.8.8.8 || { log "WARN: no internet after WiFi connect"; exit 1; }
log "WiFi connected"
