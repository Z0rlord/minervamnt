#!/usr/bin/env bash
# Build Linux ARM64 artifacts on Mac, rsync to Pi, run install-signet-stack.sh.
set -euo pipefail

PI_HOST="${PI_HOST:-z0rlord@100.96.246.94}"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/raspi_key}"
INSTALL_ROOT="${INSTALL_ROOT:-/opt/minervamnt}"
BARK_VERSION="${BARK_VERSION:-0.3.0}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
STAGE="$REPO_ROOT/target/pi-signet-stage"
DOCKER_IMAGE="${DOCKER_IMAGE:-rust:1-bookworm}"

log() { printf '[deploy-signet] %s\n' "$*"; }

ssh_pi() {
  ssh -i "$SSH_KEY" -o StrictHostKeyChecking=accept-new "$PI_HOST" "$@"
}

rsync_pi() {
  rsync -avz -e "ssh -i $SSH_KEY -o StrictHostKeyChecking=no" "$@"
}

log "repo: $REPO_ROOT"
log "target: $PI_HOST -> $INSTALL_ROOT"

build_on_pi() {
  log "building on Pi (native aarch64)"
  ssh_pi "command -v cargo >/dev/null || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y"
  ssh_pi "source \"\$HOME/.cargo/env\" 2>/dev/null; \
    sudo DEBIAN_FRONTEND=noninteractive apt-get update -qq && \
    sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libssl-dev build-essential protobuf-compiler"
  rsync_pi --delete \
    --exclude target --exclude .git --exclude data --exclude '*.sqlite' \
    "$REPO_ROOT/" "$PI_HOST:~/minervamnt-build/"
  ssh_pi "source \"\$HOME/.cargo/env\" && cd ~/minervamnt-build && \
    cargo build --release && cargo build --release --example signatory_ping && \
    cargo install cdk-signatory@0.17.1 --root ~/minervamnt-build/target/pi-signet-stage --locked"
  mkdir -p "$STAGE/bin"
  rsync_pi "$PI_HOST:~/minervamnt-build/target/release/minerva-mint" "$STAGE/bin/"
  rsync_pi "$PI_HOST:~/minervamnt-build/target/release/examples/signatory_ping" "$STAGE/bin/"
  rsync_pi "$PI_HOST:~/minervamnt-build/target/pi-signet-stage/bin/signatory" "$STAGE/bin/"
  chmod +x "$STAGE/bin/"*
}

build_with_docker() {
  log "building minerva-mint + signatory_ping (linux/arm64 via Docker)"
  mkdir -p "$STAGE/bin"
  docker run --rm --platform linux/arm64 \
    -v "$REPO_ROOT:/work" -w /work "$DOCKER_IMAGE" \
    bash -c '
      set -euo pipefail
      apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq pkg-config libssl-dev >/dev/null
      cargo build --release
      cargo build --release --example signatory_ping
      cargo install cdk-signatory@0.17.1 --root /work/target/pi-signet-stage --locked
    '
  install -m 755 "$REPO_ROOT/target/release/minerva-mint" "$STAGE/bin/minerva-mint"
  install -m 755 "$REPO_ROOT/target/release/examples/signatory_ping" "$STAGE/bin/signatory_ping"
  install -m 755 "$STAGE/bin/signatory" "$STAGE/bin/signatory" 2>/dev/null || \
    install -m 755 "$STAGE/signatory" "$STAGE/bin/signatory"
}

mkdir -p "$STAGE/bin"
if [[ "${BUILD_ON_PI:-}" == "1" ]]; then
  build_on_pi
elif docker info >/dev/null 2>&1; then
  build_with_docker
else
  log "Docker unavailable — falling back to BUILD_ON_PI=1"
  build_on_pi
fi

log "downloading barkd/bark ${BARK_VERSION} linux-arm64"
BARK_RELEASE="https://gitlab.com/ark-bitcoin/bark/-/releases/bark-${BARK_VERSION}/downloads"
curl -fsSL "$BARK_RELEASE/barkd-${BARK_VERSION}-linux-arm64" -o "$STAGE/bin/barkd"
curl -fsSL "$BARK_RELEASE/bark-${BARK_VERSION}-linux-arm64" -o "$STAGE/bin/bark"
chmod +x "$STAGE/bin/barkd" "$STAGE/bin/bark"

log "staging configs and systemd units"
mkdir -p "$STAGE/deploy/systemd" "$STAGE/deploy/pi"
cp "$REPO_ROOT/deploy/systemd/barkd-signet.service" \
   "$REPO_ROOT/deploy/systemd/cdk-signatory-signet.service" \
   "$REPO_ROOT/deploy/systemd/minerva-mint-signet.service" \
   "$STAGE/deploy/systemd/"
cp "$REPO_ROOT/deploy/pi/config.signet.pi.toml.example" \
   "$REPO_ROOT/deploy/pi/install-signet-stack.sh" \
   "$STAGE/deploy/pi/"

TS_IP="$(ssh_pi "ip -4 -o addr show tailscale0 | awk '{print \$4}' | cut -d/ -f1")"
log "Pi Tailscale IP: $TS_IP"
sed "s/TS_IP/$TS_IP/g" "$REPO_ROOT/deploy/pi/config.signet.pi.toml.example" > "$STAGE/config.signet.toml"

log "rsync to Pi (sudo mkdir + copy)"
ssh_pi "sudo mkdir -p '$INSTALL_ROOT' && sudo chown \$(whoami):\$(whoami) '$INSTALL_ROOT'"
rsync_pi "$STAGE/" "$PI_HOST:$INSTALL_ROOT/"

log "running install-signet-stack.sh on Pi"
ssh_pi "chmod +x '$INSTALL_ROOT/deploy/pi/install-signet-stack.sh' && \
  INSTALL_ROOT='$INSTALL_ROOT' BARK_VERSION='$BARK_VERSION' bash '$INSTALL_ROOT/deploy/pi/install-signet-stack.sh'"

log "verify from Mac over Tailscale"
sleep 3
curl -sf "http://${TS_IP}:3338/health" | jq .
echo
curl -sf "http://${TS_IP}:3338/v1/info" | jq '{name, pubkey, version: .version, nuts: .nuts."4"}'

log "deploy complete — http://${TS_IP}:3338"
