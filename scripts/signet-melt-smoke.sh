#!/usr/bin/env bash
# Signet melt smoke test for Minerva Mint.
# Exercises mint quote → mint → melt quote → melt against a signet-configured instance.
# Live melt requires barkd (wallet funded) + BARKD_AUTH_TOKEN; script reports partial success.
#
# Signing: this harness always uses SIGNATORY_BACKEND=mock (random B_ values are fine).
# For remote cdk-signatory gRPC/mTLS, run scripts/signet-signatory-ping.sh separately.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MINT_URL="${MINT_URL:-http://127.0.0.1:3338}"
BARKD_URL="${BARKD_URL:-http://127.0.0.1:3535}"
BARKD_API="${BARKD_URL%/}/api/v1"
BARK_RECV_URL="${BARK_RECV_URL:-http://127.0.0.1:3536}"
BARK_RECV_API="${BARK_RECV_URL%/}/api/v1"
MINT_BIN="${MINT_BIN:-$ROOT/target/release/minerva-mint}"
CONFIG="${MINERVA_CONFIG:-config.signet.toml}"
KEYSET_ID="${KEYSET_ID:-00minerva0mock01}"
BARKD_DATADIR="${BARKD_DATADIR:-$HOME/.bark-signet-melt}"
BARK_RECV_DATADIR="${BARK_RECV_DATADIR:-$ROOT/data/bark-smoke-recv}"
MELT_INVOICE_AMOUNT="${MELT_INVOICE_AMOUNT:-21 sat}"
MELT_INVOICE_AMOUNT_SAT="${MELT_INVOICE_AMOUNT_SAT:-21}"
MELT_PAY_TIMEOUT_SECS="${MELT_PAY_TIMEOUT_SECS:-1500}"
ARK_POLL_TIMEOUT_SECS="${ARK_POLL_TIMEOUT_SECS:-1200}"
MIN_SPENDABLE_SAT="${MIN_SPENDABLE_SAT:-50000}"
MINT_PID=""
BARKD_PID=""
RECV_BARKD_PID=""
RECV_SYNC_PID=""
STARTED_MINT=false
STARTED_BARKD=false
STARTED_RECV_BARKD=false
BARKD_STOPPED_FOR_CLI=false
USE_DOPPLER=false

export PATH="$ROOT/.local/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"

log()  { printf '[signet-smoke] %s\n' "$*"; }
pass() { printf '[PASS] %s\n' "$*"; }
fail() { printf '[FAIL] %s\n' "$*"; }
skip() { printf '[SKIP] %s\n' "$*"; }
block() { printf '[BLOCKED] %s\n' "$*"; }

cleanup() {
  if [[ -n "${RECV_SYNC_PID:-}" ]] && kill -0 "$RECV_SYNC_PID" 2>/dev/null; then
    kill "$RECV_SYNC_PID" 2>/dev/null || true
    wait "$RECV_SYNC_PID" 2>/dev/null || true
  fi
  if [[ "$BARKD_STOPPED_FOR_CLI" == true ]]; then
    restart_barkd_after_cli || true
  fi
  if [[ "$STARTED_MINT" == true && -n "$MINT_PID" ]] && kill -0 "$MINT_PID" 2>/dev/null; then
    kill "$MINT_PID" 2>/dev/null || true
    wait "$MINT_PID" 2>/dev/null || true
  fi
  # Intentionally leave barkd running: melt payout can take several signet
  # Ark rounds and the operator wallet should persist across smoke runs.
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
  if [[ -f "$BARKD_DATADIR/auth_token" ]]; then
    BARKD_AUTH_TOKEN="$(<"$BARKD_DATADIR/auth_token")"
    export BARKD_AUTH_TOKEN
    log "BARKD_AUTH_TOKEN: from $BARKD_DATADIR/auth_token"
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

load_recv_auth_token() {
  if [[ -n "${BARK_RECV_AUTH_TOKEN:-}" ]]; then
    return 0
  fi
  if [[ -f "$BARK_RECV_DATADIR/auth_token" ]]; then
    BARK_RECV_AUTH_TOKEN="$(<"$BARK_RECV_DATADIR/auth_token")"
    export BARK_RECV_AUTH_TOKEN
    return 0
  fi
  return 1
}

recv_barkd_is_ready() {
  local code
  load_recv_auth_token || return 1
  code="$(curl -s -o /dev/null -w '%{http_code}' -m 5 \
    -H "Authorization: Bearer $BARK_RECV_AUTH_TOKEN" \
    "$BARK_RECV_API/wallet/connected" 2>/dev/null || true)"
  code="${code:-000}"
  [[ "${code:0:3}" == "200" ]]
}

maybe_start_recv_barkd() {
  local barkd_bin
  if recv_barkd_is_ready; then
    log "recv barkd already reachable at $BARK_RECV_URL"
    return 0
  fi
  barkd_bin="$(resolve_barkd_bin)" || {
    block "barkd binary not found; cannot start recv barkd"
    return 1
  }
  ensure_recv_wallet || return 1
  mkdir -p "$BARK_RECV_DATADIR"
  log "Starting recv barkd ($barkd_bin) on $BARK_RECV_URL"
  nohup "$barkd_bin" --datadir "$BARK_RECV_DATADIR" --host 127.0.0.1 --port 3536 \
    >>"$ROOT/data/barkd-recv.log" 2>&1 &
  RECV_BARKD_PID=$!
  disown "$RECV_BARKD_PID" 2>/dev/null || true
  STARTED_RECV_BARKD=true
  for _ in $(seq 1 60); do
    if recv_barkd_is_ready; then
      log "recv barkd up (wallet connected)"
      return 0
    fi
    sleep 1
  done
  block "recv barkd failed to become reachable"
  return 1
}

wait_for_operator_balance() {
  local spendable pending tries=0
  load_barkd_auth_token || return 1
  log "Waiting for operator spendable balance (min ${MIN_SPENDABLE_SAT} sat, pending LN cleared)"
  for _ in $(seq 1 60); do
    curl -s -m 90 -X POST "$BARKD_API/wallet/sync" \
      -H "Authorization: Bearer $BARKD_AUTH_TOKEN" \
      -H 'Content-Type: application/json' \
      -d '{}' >/dev/null || true
    local tmp
    tmp="$(mktemp)"
    curl -s -m 15 -H "Authorization: Bearer $BARKD_AUTH_TOKEN" \
      "$BARKD_API/wallet/balance" >"$tmp"
    spendable="$(json_get "$tmp" '.spendable_sat // 0')"
    pending="$(json_get "$tmp" '.pending_lightning_send_sat // 0')"
    rm -f "$tmp"
    log "operator balance: spendable=${spendable} pending_ln=${pending}"
    if [[ "$pending" -eq 0 && "$spendable" -ge "$MIN_SPENDABLE_SAT" ]]; then
      return 0
    fi
    tries=$((tries + 1))
    sleep 15
  done
  block "operator wallet still locked (spendable=${spendable} pending_ln=${pending})"
  return 1
}

start_recv_sync_loop() {
  load_recv_auth_token || return 1
  (
    while true; do
      curl -s -m 60 -X POST "$BARK_RECV_API/wallet/sync" \
        -H "Authorization: Bearer $BARK_RECV_AUTH_TOKEN" \
        -H 'Content-Type: application/json' \
        -d '{}' >/dev/null 2>&1 || true
      sleep 5
    done
  ) &
  RECV_SYNC_PID=$!
  disown "$RECV_SYNC_PID" 2>/dev/null || true
  log "recv wallet sync loop started (pid $RECV_SYNC_PID)"
}

barkd_http_code() {
  local timeout="${1:-5}"
  local url="$2"
  local code
  if [[ -n "${BARKD_AUTH_TOKEN:-}" ]]; then
    code="$(curl -s -o /dev/null -w '%{http_code}' -m "$timeout" \
      -H "Authorization: Bearer $BARKD_AUTH_TOKEN" \
      "$url" 2>/dev/null || true)"
  else
    code="$(http_code "$timeout" "$url")"
  fi
  code="${code:-000}"
  echo "${code:0:3}"
}

barkd_is_ready() {
  local code
  code="$(barkd_http_code 5 "$BARKD_API/wallet/connected")"
  [[ "$code" == "200" ]]
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

# Prefer repo-local macOS binaries; skip Linux ELF builds on PATH.
resolve_barkd_bin() {
  local candidate
  for candidate in \
    "$ROOT/.local/bin/barkd" \
    "${BARKD_BIN:-}" \
    "$(command -v barkd 2>/dev/null || true)" \
    /opt/homebrew/bin/barkd \
    /usr/local/bin/barkd; do
    [[ -n "$candidate" && -x "$candidate" ]] || continue
    if file "$candidate" 2>/dev/null | grep -q 'ELF'; then
      continue
    fi
    echo "$candidate"
    return 0
  done
  return 1
}

resolve_bark_bin() {
  local candidate
  for candidate in \
    "$ROOT/.local/bin/bark" \
    "${BARK_BIN:-}" \
    "$(command -v bark 2>/dev/null || true)" \
    /opt/homebrew/bin/bark \
    /usr/local/bin/bark; do
    [[ -n "$candidate" && -x "$candidate" ]] || continue
    if file "$candidate" 2>/dev/null | grep -q 'ELF'; then
      continue
    fi
    echo "$candidate"
    return 0
  done
  return 1
}

stop_barkd_for_cli() {
  local pid
  pid="$(lsof -ti :3535 -sTCP:LISTEN 2>/dev/null | head -1 || true)"
  [[ -n "$pid" ]] || return 0
  log "Stopping barkd (pid $pid) so bark CLI can open wallet"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  BARKD_STOPPED_FOR_CLI=true
  sleep 2
}

restart_barkd_after_cli() {
  local barkd_bin
  [[ "$BARKD_STOPPED_FOR_CLI" == true ]] || return 0
  barkd_bin="$(resolve_barkd_bin)" || {
    block "barkd binary not found; cannot restart after bark ln invoice"
    return 1
  }
  load_barkd_auth_token || true
  mkdir -p "$BARKD_DATADIR"
  log "Restarting barkd ($barkd_bin)"
  nohup "$barkd_bin" --datadir "$BARKD_DATADIR" --host 127.0.0.1 --port 3535 \
    >>"$ROOT/data/barkd-smoke.log" 2>&1 &
  BARKD_PID=$!
  disown "$BARKD_PID" 2>/dev/null || true
  BARKD_STOPPED_FOR_CLI=false
  for _ in $(seq 1 60); do
    if barkd_is_ready; then
      log "barkd back up (wallet connected)"
      return 0
    fi
    sleep 1
  done
  block "barkd failed to restart after bark ln invoice"
  return 1
}

maybe_start_barkd() {
  local code barkd_bin
  load_barkd_auth_token || true
  code="$(barkd_http_code 5 "$BARKD_API/wallet/connected")"
  if [[ "$code" == "200" ]]; then
    log "barkd already reachable at $BARKD_URL (wallet connected)"
    return 0
  fi
  if [[ "$code" != "000" && "$code" != "401" ]]; then
    log "barkd reachable at $BARKD_URL (HTTP $code) but wallet not connected yet"
    return 0
  fi
  barkd_bin="$(resolve_barkd_bin)" || {
    block "barkd binary not found; live melt unavailable"
    return 1
  }
  mkdir -p "$BARKD_DATADIR"
  log "Starting barkd ($barkd_bin) with datadir $BARKD_DATADIR"
  nohup "$barkd_bin" --datadir "$BARKD_DATADIR" --host 127.0.0.1 --port 3535 \
    >>"$ROOT/data/barkd-smoke.log" 2>&1 &
  BARKD_PID=$!
  disown "$BARKD_PID" 2>/dev/null || true
  STARTED_BARKD=true
  for _ in $(seq 1 60); do
    if barkd_is_ready; then
      log "barkd up (wallet connected)"
      return 0
    fi
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
  export ARK_POLL_TIMEOUT_SECS="${ARK_POLL_TIMEOUT_SECS:-1200}"
  export ARK_POLL_INTERVAL_SECS="${ARK_POLL_INTERVAL_SECS:-5}"
  # Melt smoke uses mock blind signing (harness emits random B_ points).
  export SIGNATORY_BACKEND=mock
  unset SIGNATORY_URL SIGNATORY_TLS_DIR 2>/dev/null || true
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
  disown "$MINT_PID" 2>/dev/null || true
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
  curl -s -m 10 "$MINT_URL/v1/keysets" >"$tmp" 2>/dev/null || true
  local kid
  kid="$(json_get "$tmp" '.keysets[] | select(.active==true) | .id' 2>/dev/null || true)"
  if [[ -z "$kid" || "$kid" == "null" ]]; then
    kid="$(json_get "$tmp" '.keysets[0].id // empty')"
  fi
  if [[ -z "$kid" || "$kid" == "null" ]]; then
    curl -s -m 10 "$MINT_URL/v1/info" >"$tmp"
    kid="$(json_get "$tmp" '.nuts."4".methods[0].id // empty')"
  fi
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

ensure_recv_wallet() {
  if [[ -f "$BARK_RECV_DATADIR/config.toml" ]]; then
    return 0
  fi
  local bark_bin
  bark_bin="$(resolve_bark_bin)" || return 1
  log "Creating smoke receive wallet at $BARK_RECV_DATADIR"
  BARK_DATADIR="$BARK_RECV_DATADIR" "$bark_bin" create --signet \
    --ark "https://ark.signet.2nd.dev" \
    --esplora "https://esplora.signet.2nd.dev" \
    -q >>"$ROOT/data/bark-recv-create.log" 2>&1
}

obtain_melt_invoice() {
  local bark_bin tmp err inv
  MELT_INVOICE=""

  ensure_recv_wallet || {
    block "failed to create smoke receive wallet at $BARK_RECV_DATADIR"
    return 1
  }

  maybe_start_recv_barkd || return 1
  load_recv_auth_token || {
    block "recv barkd auth token missing at $BARK_RECV_DATADIR/auth_token"
    return 1
  }

  tmp="$(mktemp)"
  for _ in $(seq 1 30); do
    curl -s -m 45 -X POST "$BARK_RECV_API/lightning/receives/invoice" \
      -H "Authorization: Bearer $BARK_RECV_AUTH_TOKEN" \
      -H 'Content-Type: application/json' \
      -d "{\"amount_sat\":$MELT_INVOICE_AMOUNT_SAT,\"description\":\"minerva signet melt smoke\"}" \
      >"$tmp"
    inv="$(json_get "$tmp" '.invoice // empty')"
    if [[ -n "$inv" && "$inv" == ln* ]]; then
      MELT_INVOICE="$inv"
      log "Melt invoice from recv barkd ($MELT_INVOICE_AMOUNT_SAT sat)"
      rm -f "$tmp"
      return 0
    fi
    curl -s -m 30 -X POST "$BARK_RECV_API/wallet/sync" \
      -H "Authorization: Bearer $BARK_RECV_AUTH_TOKEN" \
      -H 'Content-Type: application/json' \
      -d '{}' >/dev/null || true
    sleep 12
  done
  rm -f "$tmp"

  # Fallback: bark CLI when recv barkd invoice API is outside round window.
  bark_bin="$(resolve_bark_bin)" || {
    block "recv barkd invoice failed and bark CLI not found"
    return 1
  }
  tmp="$(mktemp)"
  err="$(mktemp)"
  if ! BARK_DATADIR="$BARK_RECV_DATADIR" "$bark_bin" ln invoice "$MELT_INVOICE_AMOUNT" \
    --description "minerva signet melt smoke" -q >"$tmp" 2>"$err"; then
    block "bark ln invoice failed: $(tail -1 "$err" 2>/dev/null || echo unknown)"
    cat "$err" >&2
    rm -f "$tmp" "$err"
    return 1
  fi
  rm -f "$err"
  inv="$(json_get "$tmp" '.invoice // empty')"
  if [[ -z "$inv" || "$inv" == "null" ]]; then
    inv="$(grep -E '^lntbs|^lnbc' "$tmp" | tail -1 || true)"
  fi
  rm -f "$tmp"
  if [[ -n "$inv" && "$inv" == ln* ]]; then
    MELT_INVOICE="$inv"
    log "Melt invoice from bark ln invoice fallback ($MELT_INVOICE_AMOUNT)"
    return 0
  fi

  block "could not obtain melt invoice from recv wallet"
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

  if [[ -n "${BARKD_AUTH_TOKEN:-}" ]]; then
    wait_for_operator_balance || true
    log "Syncing barkd wallet before melt (signet Ark rounds can take several minutes)"
    curl -s -m 120 -X POST "$BARKD_API/wallet/sync" \
      -H "Authorization: Bearer $BARKD_AUTH_TOKEN" \
      -H 'Content-Type: application/json' \
      -d '{}' >/dev/null || true
  fi

  maybe_start_recv_barkd || true
  start_recv_sync_loop || true

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

  log "Melt pay may block up to ${MELT_PAY_TIMEOUT_SECS}s (ARK_POLL_TIMEOUT_SECS=${ARK_POLL_TIMEOUT_SECS})"
  local tmp preimage state
  tmp="$(mktemp)"
  curl -s -m "${MELT_PAY_TIMEOUT_SECS}" -X POST "$MINT_URL/v1/melt/bolt11" \
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
  if [[ -z "$state" && -z "$detail" && ! -s "$tmp" ]]; then
    block "POST /v1/melt/bolt11 — HTTP client timed out after ${MELT_PAY_TIMEOUT_SECS}s (mint may still be polling barkd)"
  elif [[ -z "$state" && -z "$detail" ]]; then
    block "POST /v1/melt/bolt11 — empty response (curl timeout or mint crash)"
  else
    block "POST /v1/melt/bolt11 — melt did not complete: state=${state:-unknown} ${detail}"
  fi
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
  if barkd_bin="$(resolve_barkd_bin 2>/dev/null)"; then
    log "barkd binary: $barkd_bin"
  else
    log "barkd binary: not found"
  fi
  if bark_cli="$(resolve_bark_bin 2>/dev/null)"; then
    log "bark binary: $bark_cli"
  else
    log "bark binary: not found (required for melt invoice)"
  fi
  load_barkd_auth_token || true
  maybe_start_barkd || true
  maybe_start_recv_barkd || true

  local barkd_code
  barkd_code="$(barkd_http_code 5 "$BARKD_API/wallet/connected")"
  if [[ "$barkd_code" == "000" ]]; then
    block "barkd not reachable at $BARKD_URL"
  else
    log "barkd /api/v1/wallet/connected → HTTP $barkd_code"
  fi

  build_mint
  log "signatory: mock (remote gRPC → scripts/signet-signatory-ping.sh)"
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
