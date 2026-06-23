#!/usr/bin/env bash
# Verify remote cdk-signatory gRPC + mTLS (separate from melt smoke, which uses mock signing).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

export PATH="$ROOT/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
export SIGNATORY_URL="${SIGNATORY_URL:-https://localhost:3340}"
export SIGNATORY_TLS_DIR="${SIGNATORY_TLS_DIR:-$ROOT/data/cdk-signatory-signet}"

log()  { printf '[signatory-ping] %s\n' "$*"; }
pass() { printf '[PASS] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*"; exit 1; }

log "=== Remote CDK signatory ping ==="
log "URL=$SIGNATORY_URL TLS_DIR=$SIGNATORY_TLS_DIR"

bash "$ROOT/scripts/start-cdk-signatory-signet.sh" || fail "start-cdk-signatory-signet.sh failed"

out="$(mktemp)"
if ! TLS_DIR="$SIGNATORY_TLS_DIR" URL="$SIGNATORY_URL" \
  cargo run --release -q --example signatory_ping >"$out" 2>&1; then
  cat "$out" >&2
  rm -f "$out"
  fail "signatory_ping exited non-zero"
fi

if ! grep -q '^connected:' "$out"; then
  cat "$out" >&2
  rm -f "$out"
  fail "signatory_ping did not report a connection"
fi

keysets="$(grep -c '^keyset id=' "$out" || true)"
pubkey="$(grep '^pubkey=' "$out" | head -1 | cut -d= -f2-)"
rm -f "$out"

if [[ "$keysets" -lt 1 ]]; then
  fail "signatory returned no active keysets"
fi

pass "gRPC + mTLS OK — pubkey=${pubkey:0:16}… keysets=$keysets"
