#!/usr/bin/env bash
set -uo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LOG="/tmp/pi-recover-$(date +%Y%m%d%H%M%S).log"
exec > >(tee "$LOG") 2>&1
cd "$REPO_ROOT"
printf '[runner] log=%s repo=%s\n' "$LOG" "$REPO_ROOT"
printf 'y\n\n' | bash deploy/pi/recover-boot-drive-mac.sh
echo "[runner] exit=$?"
