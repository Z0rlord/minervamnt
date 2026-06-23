#!/usr/bin/env bash
# Generate mTLS certs (if missing), start cdk-signatory, bootstrap sat keyset when empty.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export PATH="$ROOT/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

WORKDIR="${SIGNATORY_WORK_DIR:-$ROOT/data/cdk-signatory-signet}"
PORT="${SIGNATORY_PORT:-3340}"
URL="${SIGNATORY_URL:-https://localhost:3340}"
LOG="$ROOT/data/cdk-signatory.log"

log() { printf '[signatory] %s\n' "$*"; }

ensure_certs() {
  mkdir -p "$WORKDIR"
  if [[ -f "$WORKDIR/ca.pem" && -f "$WORKDIR/server.pem" && -f "$WORKDIR/client.pem" ]]; then
    return 0
  fi
  log "generating mTLS certs in $WORKDIR"
  openssl genrsa -out "$WORKDIR/ca.key" 4096
  openssl req -x509 -new -nodes -key "$WORKDIR/ca.key" -sha256 -days 825 \
    -subj "/CN=Minerva Signet Signatory CA" -out "$WORKDIR/ca.pem"
  cat > "$WORKDIR/server.ext" <<'EOF'
subjectAltName=DNS:localhost,IP:127.0.0.1
EOF
  openssl genrsa -out "$WORKDIR/server.key" 2048
  openssl req -new -key "$WORKDIR/server.key" -out "$WORKDIR/server.csr" -subj "/CN=localhost"
  openssl x509 -req -in "$WORKDIR/server.csr" -CA "$WORKDIR/ca.pem" -CAkey "$WORKDIR/ca.key" \
    -CAcreateserial -out "$WORKDIR/server.pem" -days 825 -sha256 -extfile "$WORKDIR/server.ext"
  openssl genrsa -out "$WORKDIR/client.key" 2048
  openssl req -new -key "$WORKDIR/client.key" -out "$WORKDIR/client.csr" -subj "/CN=minerva-mint"
  openssl x509 -req -in "$WORKDIR/client.csr" -CA "$WORKDIR/ca.pem" -CAkey "$WORKDIR/ca.key" \
    -CAcreateserial -out "$WORKDIR/client.pem" -days 825 -sha256
  rm -f "$WORKDIR/server.csr" "$WORKDIR/client.csr"
}

ensure_signatory() {
  if lsof -i ":$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    log "signatory already listening on :$PORT"
    return 0
  fi
  command -v signatory >/dev/null || {
    log "installing cdk-signatory 0.17.1"
    cargo install cdk-signatory@0.17.1 --root "$ROOT/.local"
  }
  log "starting signatory on 127.0.0.1:$PORT (work-dir=$WORKDIR)"
  nohup signatory --enable-logging --log-level info \
    --work-dir "$WORKDIR" \
    --listen-addr 127.0.0.1 --listen-port "$PORT" \
    >>"$LOG" 2>&1 &
  for _ in $(seq 1 30); do
    lsof -i ":$PORT" -sTCP:LISTEN >/dev/null 2>&1 && return 0
    sleep 1
  done
  log "signatory failed to bind — tail $LOG"
  tail -20 "$LOG" || true
  exit 1
}

bootstrap_keyset() {
  cargo build --release --example signatory_ping >/dev/null
  TLS_DIR="$WORKDIR" URL="$URL" BOOTSTRAP=1 \
    "$ROOT/target/release/examples/signatory_ping"
}

ensure_certs
ensure_signatory
bootstrap_keyset
log "ready: $URL (tls_dir=$WORKDIR)"
