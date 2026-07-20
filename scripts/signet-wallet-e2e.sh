#!/usr/bin/env bash
# Real Cashu wallet mint→melt e2e against a *running* Minerva Mint with remote
# cdk-signatory (valid BDHKE blinds). Does NOT use SIGNATORY_BACKEND=mock.
#
# Prerequisites (already running):
#   barkd operator :3535, barkd recv :3536, cdk-signatory :3340, mint :3338
#
# Usage:
#   bash scripts/signet-wallet-e2e.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MINT_URL="${MINT_URL:-http://127.0.0.1:3338}"
BARK_RECV_URL="${BARK_RECV_URL:-http://127.0.0.1:3536}"
BARK_RECV_API="${BARK_RECV_URL%/}/api/v1"
BARK_RECV_DATADIR="${BARK_RECV_DATADIR:-$ROOT/data/bark-smoke-recv}"
SIGNATORY_URL="${SIGNATORY_URL:-https://localhost:3340}"
SIGNATORY_TLS_DIR="${SIGNATORY_TLS_DIR:-$ROOT/data/cdk-signatory-signet}"
MELT_INVOICE_AMOUNT_SAT="${MELT_INVOICE_AMOUNT_SAT:-21}"

export PATH="$ROOT/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

log()  { printf '[wallet-e2e] %s\n' "$*"; }
pass() { printf '[PASS] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*"; exit 1; }

json_field() {
  python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get(sys.argv[1],"") or "")' "$1"
}

log "=== Real wallet mint→melt e2e (remote signatory) ==="
log "MINT_URL=$MINT_URL SIGNATORY_URL=$SIGNATORY_URL"

code="$(curl -s -m 10 -o /dev/null -w '%{http_code}' "$MINT_URL/health" || true)"
[[ "$code" == "200" ]] || fail "mint not healthy at $MINT_URL (HTTP $code) — start stack first"

[[ -f "$BARK_RECV_DATADIR/auth_token" ]] || fail "recv auth_token missing: $BARK_RECV_DATADIR/auth_token"
BARK_RECV_AUTH_TOKEN="$(<"$BARK_RECV_DATADIR/auth_token")"

MELT_INVOICE=""
for _ in $(seq 1 20); do
  inv="$(curl -s -m 45 -X POST "$BARK_RECV_API/lightning/receives/invoice" \
    -H "Authorization: Bearer $BARK_RECV_AUTH_TOKEN" \
    -H 'Content-Type: application/json' \
    -d "{\"amount_sat\":$MELT_INVOICE_AMOUNT_SAT,\"description\":\"minerva wallet e2e\"}" \
    | json_field invoice)"
  if [[ -n "$inv" && "$inv" == ln* ]]; then
    MELT_INVOICE="$inv"
    break
  fi
  curl -s -m 30 -X POST "$BARK_RECV_API/wallet/sync" \
    -H "Authorization: Bearer $BARK_RECV_AUTH_TOKEN" \
    -H 'Content-Type: application/json' \
    -d '{}' >/dev/null || true
  sleep 8
done
[[ -n "$MELT_INVOICE" ]] || fail "could not obtain melt invoice from recv barkd :3536"
pass "melt invoice (${MELT_INVOICE_AMOUNT_SAT} sat)"

# Operator wallet sync before melt (ASP can briefly mark vtxos spent mid-pay).
# Funded operator datadir for this session — not melt-fresh.
BARKD_DATADIR="${BARKD_DATADIR:-$HOME/.bark-signet-melt}"
[[ -f "$BARKD_DATADIR/auth_token" ]] || fail "operator auth_token missing: $BARKD_DATADIR/auth_token"
OP_TOK="$(<"$BARKD_DATADIR/auth_token")"
op_code="$(curl -s -m 5 -o /dev/null -w '%{http_code}' \
  -H "Authorization: Bearer $OP_TOK" \
  http://127.0.0.1:3535/api/v1/wallet/connected || true)"
[[ "$op_code" == "200" ]] || fail "operator barkd :3535 not up (HTTP $op_code) — datadir $BARKD_DATADIR"
curl -s -m 120 -X POST "http://127.0.0.1:3535/api/v1/wallet/sync" \
  -H "Authorization: Bearer $OP_TOK" \
  -H 'Content-Type: application/json' \
  -d '{}' >/dev/null || true
bal="$(curl -s -m 15 -H "Authorization: Bearer $OP_TOK" \
  http://127.0.0.1:3535/api/v1/wallet/balance || true)"
log "operator balance [${BARKD_DATADIR}]: ${bal:-unavailable}"

log "Running examples/wallet_e2e (prebuilt release)..."
export MINT_URL SIGNATORY_URL SIGNATORY_TLS_DIR MELT_INVOICE
set +e
if [[ -x "$ROOT/target/release/examples/wallet_e2e" ]]; then
  "$ROOT/target/release/examples/wallet_e2e"
  rc=$?
else
  cargo run --release -q --example wallet_e2e
  rc=$?
fi
set -e
[[ $rc -eq 0 ]] || fail "wallet_e2e exited $rc"
pass "wallet e2e complete"
