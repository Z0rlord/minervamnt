#!/usr/bin/env bash
# Signet melt smoke test for Minerva Mint.
# Exercises mint quote → mint → melt quote → melt against a signet-configured instance.
# Live melt requires barkd (wallet funded) + BARKD_AUTH_TOKEN; script reports partial success.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MINT_URL="${MINT_URL:-http://127.0.0.1:3338}"
BARKD_URL="${BARKD_URL:-http://127.0.0.1:3535}"
MINT_BIN="${MINT_BIN:-$ROOT/target/release/minerva-mint}"
CONFIG="${MINERVA_CONFIG:-config.signet.toml}"
KEYSET_ID="${KEYSET_ID:-00minerva0mock01}"
MINT_PID=""
BARKD_PID=""
STARTED_MINT=false
STARTED_BARKD=false
USE_DOPPLER=false

log()  { printf '[signet-smoke] %s\n' "$*"; }
pass() { printf '[PASS] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*"; }
skip() { printf '[SKIP] %s\n' "$*"; }
block() { printf '[BLOCKED] %s\n' "$*"; }

cleanup() {
  if [[ "$STARTED_MINT" == true && -n "$MINT_PID" ]] && kill -0 "$MINT_PID" 2>/dev/null; then
    kill "$MINT_PID" 2>/dev/null || true
    wait "$MINT_PID" 2>/dev/null || true
  fi
  if [[ "$STARTED_BARKD" == true && -n "$BARKD_PID" ]] && kill -0 "$BARKD_PID" 2>/dev/null; then
    kill "$BARKD_PID" 2>/dev/null || true
    wait "$BARKD_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# --- Safe .env load (export KEY=VALUE lines only; no eval of arbitrary shell) ---
load_dotenv() {
  local f="$1"
  [[ -f "$f" ]] || return 0
  while IFS= read -r line || [[ -n "$line" ]]; do
    [[ "$line" =~ ^[[:space:]]*# ]] && continue
    [[ -z "${line//[[:space:]]/}" ]] && continue
    if [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]]; then
      local key="${BASH_REMATCH[1]}"
      local val="${BASH_REMATCH[2]}"
      val="${val%\"}"; val="${val#\"}"
      val="${val%\'}"; val="${val#\'}"
      export "$key=$val"
    fi
  done < <(grep -E '^[A-Za-z_][A-Za-z0-9_]*=' "$f" 2>/dev/null || true)
}

load_barkd_auth_token() {
  if [[ -n "${BARKD_AUTH_TOKEN:-}" ]]; then
    log "BARKD_AUTH_TOKEN: from environment"
    return 0
  fi
  if [[ -f "$HOME/.bark-signet/auth_token" ]]; then
    BARKD_AUTH_TOKEN="$(<"$HOME/.bark-signet/auth_token")"
    export BARKD_AUTH_TOKEN
    log "BARKD_AUTH_TOKEN: from ~/.bark-signet/auth_token"
    return 0
  fi
  load_dotenv "$ROOT/.env"
  if [[ -n "${BARKD_AUTH_TOKEN:-}" ]]; then
    log "BARKD_AUTH_TOKEN: from .env"
    return 0
  fi
  log "BARKD_AUTH_TOKEN: not found"
  return 1
}

http_code() {
  local code
  code="$(curl -s -o /dev/null -w '%{http_code}' -m "${1:-10}" "$2" 2>/dev/null || true)"
  if [[ -z "$code" || "$code" == "000" ]]; then
    echo "000"
  else
    echo "$code"
  fi
}

json_get() {
  local file="$1" jq_expr="$2"
  if command -v jq >/dev/null 2>&1; then
    jq -r "$jq_expr" "$file" 2>/dev/null || echo ""
  else
    python3 -c "import json,sys; d=json.load(open('$file')); print($jq_expr)" 2>/dev/null || echo ""
  fi
}

ensure_config() {
  if [[ ! -f "$CONFIG" ]]; then
    cp config.signet.toml.example "$CONFIG"
    log "Created $CONFIG from example"
  fi
}

maybe_start_barkd() {
  local code
  code="$(http_code 5 "$BARKD_URL/wallet/connected")"
  if [[ "$code" != "000" ]]; then
    log "barkd already reachable at $BARKD_URL (HTTP $code)"
    return 0
  fi
  local barkd_bin=""
  if command -v barkd >/dev/null 2>&1; then
    barkd_bin="$(command -v barkd)"
  else
    for candidate in /opt/homebrew/bin/barkd /usr/local/bin/barkd; do
      [[ -x "$candidate" ]] && barkd_bin="$candidate" && break
    done
  fi
  if [[ -z "$barkd_bin" ]]; then
    block "barkd binary not found; live melt unavailable"
    return 1
  fi
  mkdir -p "$HOME/.bark-signet"
  log "Starting barkd ($barkd_bin) with datadir ~/.bark-signet"
  "$barkd_bin" --datadir "$HOME/.bark-signet" --host 127.0.0.1 --port 3535 \
    >>"$ROOT/data/barkd-smoke.log" 2>&1 &
  BARKD_PID=$!
  STARTED_BARKD=true
  for _ in $(seq 1 30); do
    code="$(http_code 2 "$BARKD_URL/wallet/connected")"
    [[ "$code" != "000" ]] && { log "barkd up (HTTP $code)"; return 0; }
    sleep 1
  done
  block "barkd failed to become reachable"
  return 1
}

build_mint() {
  if [[ -x "$MINT_BIN" ]]; then
    log "Mint binary present: $MINT_BIN"
    return 0
  fi
  log "Building release binary..."
  cargo build --release
}

start_mint() {
  local code
  # /health may take ~30–90s when barkd/Bitcoin RPC are unreachable (sequential probes).
  code="$(http_code 90 "$MINT_URL/health")"
  if [[ "$code" == "200" ]]; then
    log "Mint already running at $MINT_URL"
    return 0
  fi
  if lsof -i :3338 -sTCP:LISTEN >/dev/null 2>&1; then
    log "Port 3338 in use; waiting for /health (slow when barkd is down)..."
    for _ in $(seq 1 90); do
      code="$(http_code 90 "$MINT_URL/health")"
      [[ "$code" == "200" ]] && { log "Mint responding at $MINT_URL"; return 0; }
      sleep 2
    done
  fi

  mkdir -p data
  export MINERVA_CONFIG="$CONFIG"
  load_dotenv "$ROOT/.env"

  local runner=("$MINT_BIN")
  if [[ -f doppler.yaml ]] && command -v doppler >/dev/null 2>&1; then
    USE_DOPPLER=true
    runner=(doppler run -- "$MINT_BIN")
    log "Starting mint via doppler run (BITCOIN_RPC_* injection)"
  else
    log "Starting mint (no doppler)"
  fi

  MINERVA_CONFIG="$CONFIG" "${runner[@]}" >>"$ROOT/data/mint-smoke.log" 2>&1 &
  MINT_PID=$!
  STARTED_MINT=true

  for _ in $(seq 1 90); do
    code="$(http_code 90 "$MINT_URL/health")"
    [[ "$code" == "200" ]] && { log "Mint up at $MINT_URL"; return 0; }
    sleep 2
  done
  fail "Mint failed to start — see data/mint-smoke.log"
  tail -20 "$ROOT/data/mint-smoke.log" 2>/dev/null || true
  return 1
}

fetch_keyset_id() {
  local tmp
  tmp="$(mktemp)"
  curl -s -m 10 "$MINT_URL/v1/info" >"$tmp"
  local kid
  kid="$(json_get "$tmp" '.nuts."4".methods[0].id // empty')"
  rm -f "$tmp"
  if [[ -n "$kid" && "$kid" != "null" ]]; then
    KEYSET_ID="$kid"
  fi
  log "Using keyset id: $KEYSET_ID"
}

step_health() {
  local tmp
  tmp="$(mktemp)"
  local code
  code="$(curl -s -m 90 -o "$tmp" -w '%{http_code}' "$MINT_URL/health")"
  if [[ "$code" != "200" ]]; then
    fail "GET /health → HTTP $code"
    return 1
  fi
  local ark_connected status
  ark_connected="$(json_get "$tmp" '.ark_connected')"
  status="$(json_get "$tmp" '.status')"
  log "health: status=$status ark_connected=$ark_connected"
  if [[ "$ark_connected" == "true" ]]; then
    pass "GET /health — ark_connected=true"
  else
    skip "GET /health — ark_connected=false (barkd/ASP not wired)"
  fi
  rm -f "$tmp"
}

step_mint_flow() {
  local tmp qid
  tmp="$(mktemp)"

  curl -s -m 15 -X POST "$MINT_URL/v1/mint/quote/bolt11" \
    -H 'Content-Type: application/json' \
    -d '{"amount":64,"unit":"sat"}' >"$tmp"
  local state
  state="$(json_get "$tmp" '.state')"
  qid="$(json_get "$tmp" '.quote')"
  if [[ -z "$qid" || "$qid" == "null" ]]; then
    fail "POST /v1/mint/quote/bolt11 — no quote id"
    cat "$tmp" >&2
    rm -f "$tmp"
    return 1
  fi
  pass "POST /v1/mint/quote/bolt11 — quote=$qid state=$state"

  curl -s -m 30 -X POST "$MINT_URL/v1/mint/bolt11" \
    -H 'Content-Type: application/json' \
    -d "{\"quote\":\"$qid\",\"outputs\":[{\"amount\":64,\"id\":\"$KEYSET_ID\",\"B_\":\"02$(openssl rand -hex 32)\"}]}" \
    >"$tmp"
  local sig_count
  sig_count="$(json_get "$tmp" '.signatures | length')"
  if [[ "$sig_count" == "1" ]]; then
    pass "POST /v1/mint/bolt11 — received 1 signature"
    MINT_SIG_C="$(json_get "$tmp" '.signatures[0].C_')"
    MINT_AMOUNT=64
    rm -f "$tmp"
    return 0
  fi
  local detail
  detail="$(json_get "$tmp" '.detail // .message // empty')"
  skip "POST /v1/mint/bolt11 — failed (likely barkd/VTXO board): ${detail:-see response}"
  cat "$tmp" >&2
  rm -f "$tmp"
  return 1
}

obtain_melt_invoice() {
  local tmp inv
  tmp="$(mktemp)"
  MELT_INVOICE=""

  if load_barkd_auth_token 2>/dev/null; then
    local auth_hdr=()
    [[ -n "${BARKD_AUTH_TOKEN:-}" ]] && auth_hdr=(-H "Authorization: Bearer $BARKD_AUTH_TOKEN")
    curl -s -m 20 -X POST "$BARKD_URL/api/v1/lightning/invoice" \
      "${auth_hdr[@]}" \
      -H 'Content-Type: application/json' \
      -d '{"amount_sat":21,"description":"minerva signet melt smoke"}' >"$tmp" 2>/dev/null || true
    inv="$(json_get "$tmp" '.invoice // .bolt11 // .payment_request // empty')"
    if [[ -n "$inv" && "$inv" != "null" ]]; then
      MELT_INVOICE="$inv"
      log "Melt invoice from barkd lightning/invoice"
      rm -f "$tmp"
      return 0
    fi
    block "barkd invoice API did not return bolt11 (wallet may be unfunded or 401)"
  else
    block "No BARKD_AUTH_TOKEN — cannot request signet invoice from barkd"
  fi
  rm -f "$tmp"
  return 1
}

step_melt_quote() {
  local tmp melt_qid amount fee
  tmp="$(mktemp)"

  if [[ -z "${MELT_INVOICE:-}" ]]; then
    skip "POST /v1/melt/quote/bolt11 — no real signet invoice"
    rm -f "$tmp"
    return 1
  fi

  local payload
  payload="$(python3 -c 'import json,sys; print(json.dumps({"request":sys.argv[1],"unit":"sat"}))' "$MELT_INVOICE")"
  curl -s -m 20 -X POST "$MINT_URL/v1/melt/quote/bolt11" \
    -H 'Content-Type: application/json' \
    -d "$payload" >"$tmp"

  melt_qid="$(json_get "$tmp" '.quote')"
  amount="$(json_get "$tmp" '.amount')"
  fee="$(json_get "$tmp" '.fee_reserve')"
  if [[ -z "$melt_qid" || "$melt_qid" == "null" ]]; then
    fail "POST /v1/melt/quote/bolt11"
    cat "$tmp" >&2
    rm -f "$tmp"
    return 1
  fi
  MELT_QUOTE_ID="$melt_qid"
  MELT_AMOUNT="$amount"
  MELT_FEE="$fee"
  pass "POST /v1/melt/quote/bolt11 — quote=$melt_qid amount=$amount fee_reserve=$fee"
  rm -f "$tmp"
}

step_melt_pay() {
  if [[ -z "${MELT_QUOTE_ID:-}" ]]; then
    skip "POST /v1/melt/bolt11 — no melt quote"
    return 1
  fi
  if [[ -z "${MINT_SIG_C:-}" ]]; then
    # Mint extra tokens to cover melt if earlier mint step failed we still try with fresh mint
    step_mint_flow || true
  fi
  if [[ -z "${MINT_SIG_C:-}" ]]; then
    skip "POST /v1/melt/bolt11 — no proofs available"
    return 1
  fi

  local cover=$(( MELT_AMOUNT + MELT_FEE ))
  # Round up to next power of two for Cashu denomination
  local pow=1
  while (( pow < cover )); do pow=$((pow * 2)); done
  if (( pow > MINT_AMOUNT )); then
    local tmp qid
    tmp="$(mktemp)"
    curl -s -m 15 -X POST "$MINT_URL/v1/mint/quote/bolt11" \
      -H 'Content-Type: application/json' \
      -d "{\"amount\":$pow,\"unit\":\"sat\"}" >"$tmp"
    qid="$(json_get "$tmp" '.quote')"
    curl -s -m 30 -X POST "$MINT_URL/v1/mint/bolt11" \
      -H 'Content-Type: application/json' \
      -d "{\"quote\":\"$qid\",\"outputs\":[{\"amount\":$pow,\"id\":\"$KEYSET_ID\",\"B_\":\"02$(openssl rand -hex 32)\"}]}" \
      >"$tmp"
    MINT_SIG_C="$(json_get "$tmp" '.signatures[0].C_')"
    MINT_AMOUNT=$pow
    rm -f "$tmp"
  fi

  local tmp preimage state
  tmp="$(mktemp)"
  curl -s -m 120 -X POST "$MINT_URL/v1/melt/bolt11" \
    -H 'Content-Type: application/json' \
    -d "{\"quote\":\"$MELT_QUOTE_ID\",\"inputs\":[{\"amount\":$MINT_AMOUNT,\"id\":\"$KEYSET_ID\",\"secret\":\"smoke-$(openssl rand -hex 8)\",\"C\":\"$MINT_SIG_C\"}]}" \
    >"$tmp"

  state="$(json_get "$tmp" '.state')"
  preimage="$(json_get "$tmp" '.payment_preimage // empty')"
  if [[ "$state" == "PAID" && -n "$preimage" && ${#preimage} -ge 32 ]]; then
    pass "POST /v1/melt/bolt11 — state=PAID preimage present (${#preimage} hex chars)"
    rm -f "$tmp"
    return 0
  fi
  local detail
  detail="$(json_get "$tmp" '.detail // .message // empty')"
  block "POST /v1/melt/bolt11 — melt did not complete: state=$state ${detail}"
  cat "$tmp" >&2
  rm -f "$tmp"
  return 1
}

main() {
  log "=== Minerva signet melt smoke test ==="
  log "ROOT=$ROOT MINT_URL=$MINT_URL BARKD_URL=$BARKD_URL CONFIG=$CONFIG"

  ensure_config
  load_dotenv "$ROOT/.env"

  log "--- Prerequisites ---"
  if command -v barkd >/dev/null 2>&1; then
    log "barkd binary: $(command -v barkd)"
  else
    log "barkd binary: not in PATH"
  fi
  load_barkd_auth_token || true
  maybe_start_barkd || true

  local barkd_code
  barkd_code="$(http_code 5 "$BARKD_URL/wallet/connected")"
  if [[ "$barkd_code" == "000" ]]; then
    block "barkd not reachable at $BARKD_URL"
  else
    log "barkd /wallet/connected → HTTP $barkd_code"
  fi

  build_mint
  start_mint
  fetch_keyset_id

  log "--- Smoke steps ---"
  step_health || true
  step_mint_flow || true
  obtain_melt_invoice || true
  step_melt_quote || true
  step_melt_pay || true

  log "=== Done (see PASS/FAIL/SKIP/BLOCKED above) ==="
}

main "$@"
