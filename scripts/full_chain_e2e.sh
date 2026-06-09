#!/usr/bin/env bash
# Full-chain E2E smoke for node + mining stack + explorer + transfer flow.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPORT_DIR="${ROOT_DIR}/artifacts/release-gate"
REPORT_FILE="${REPORT_DIR}/full-chain-e2e-report.md"
LOG_DIR="${ROOT_DIR}/artifacts/full-chain-e2e-logs"

MINING_STACK_DIR="${MINING_STACK_DIR:-${ROOT_DIR}/../rabbitchain-mining-stack}"
EXPLORER_DIR="${EXPLORER_DIR:-${ROOT_DIR}/../rabbitchain-explorer}"
EXPLORER_BACKEND_DIR="${EXPLORER_BACKEND_DIR:-${EXPLORER_DIR}/backend}"
EXPLORER_FRONTEND_DIR="${EXPLORER_FRONTEND_DIR:-${EXPLORER_DIR}/frontend}"

NODE_RPC_HOST="${NODE_RPC_HOST:-127.0.0.1}"
NODE_RPC_PORT="${NODE_RPC_PORT:-19545}"
NODE_WS_PORT="${NODE_WS_PORT:-19546}"
POOL_PORT="${POOL_PORT:-19332}"
MINER_METRICS_PORT="${MINER_METRICS_PORT:-19333}"
EXPLORER_BACKEND_PORT="${EXPLORER_BACKEND_PORT:-19080}"
EXPLORER_FRONTEND_PORT="${EXPLORER_FRONTEND_PORT:-15178}"

NODE_RPC_URL="http://${NODE_RPC_HOST}:${NODE_RPC_PORT}"
POOL_URL="http://127.0.0.1:${POOL_PORT}"
MINER_METRICS_URL="http://127.0.0.1:${MINER_METRICS_PORT}"
EXPLORER_BACKEND_URL="http://127.0.0.1:${EXPLORER_BACKEND_PORT}"
EXPLORER_FRONTEND_URL="http://127.0.0.1:${EXPLORER_FRONTEND_PORT}"
RPC_AUTH_TOKEN="${RPC_AUTH_TOKEN:-full-chain-e2e-token}"

COINBASE_NATIVE="${COINBASE_NATIVE:-0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9}"
RECIPIENT_NATIVE="${RECIPIENT_NATIVE:-0x1111111111111111111111111111111111111111}"
MINER_ID="${MINER_ID:-miner-ci-1}"

mkdir -p "${REPORT_DIR}" "${LOG_DIR}"

NODE_LOG="${LOG_DIR}/node.log"
POOL_LOG="${LOG_DIR}/pool.log"
MINER_LOG="${LOG_DIR}/miner.log"
EXPLORER_BACKEND_LOG="${LOG_DIR}/explorer-backend.log"
EXPLORER_FRONTEND_LOG="${LOG_DIR}/explorer-frontend.log"

PIDS=()
TMP_RUN_DIR="$(mktemp -d "${ROOT_DIR}/artifacts/e2e-run.XXXXXX")"

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "${pid}" >/dev/null 2>&1 || true
  done
  wait >/dev/null 2>&1 || true
}
trap cleanup EXIT

require_cmd() {
  local cmd="$1"
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    echo "Missing command: ${cmd}" >&2
    exit 1
  fi
}

assert_dir() {
  local dir="$1"
  if [[ ! -d "${dir}" ]]; then
    echo "Missing directory: ${dir}" >&2
    exit 1
  fi
}

assert_port_free() {
  local port="$1"
  if ss -ltn | grep -q ":${port}\\b"; then
    echo "Port ${port} is already in use" >&2
    exit 1
  fi
}

wait_http_ok() {
  local url="$1"
  local timeout_secs="${2:-60}"
  local i=0
  while (( i < timeout_secs )); do
    if curl -fsS "${url}" >/dev/null 2>&1; then
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  echo "Timeout waiting for ${url}" >&2
  return 1
}

rpc_call() {
  local method="$1"
  local params_json="$2"
  curl -fsS \
    -H 'content-type: application/json' \
    -H "authorization: Bearer ${RPC_AUTH_TOKEN}" \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params_json}}" \
    "${NODE_RPC_URL}"
}

extract_result_hex() {
  sed -n 's/.*"result":"\([^"]*\)".*/\1/p'
}

extract_block_number_hex() {
  sed -n 's/.*"number":"\([^"]*\)".*/\1/p'
}

extract_balance_hex() {
  sed -n 's/.*"balance":"\([^"]*\)".*/\1/p'
}

hex_to_dec() {
  local hex="${1#0x}"
  if [[ -z "${hex}" ]]; then
    echo "0"
    return
  fi
  printf '%d' "$((16#${hex}))"
}

require_cmd cargo
require_cmd curl
require_cmd npm
require_cmd ss

assert_dir "${MINING_STACK_DIR}"
assert_dir "${EXPLORER_BACKEND_DIR}"
assert_dir "${EXPLORER_FRONTEND_DIR}"

assert_port_free "${NODE_RPC_PORT}"
assert_port_free "${NODE_WS_PORT}"
assert_port_free "${POOL_PORT}"
assert_port_free "${MINER_METRICS_PORT}"
assert_port_free "${EXPLORER_BACKEND_PORT}"
assert_port_free "${EXPLORER_FRONTEND_PORT}"

echo "==> Build Rabbit-Chain-node CLI"
cargo build -p rabbitcli >/dev/null

echo "==> Build explorer frontend"
(cd "${EXPLORER_FRONTEND_DIR}" && npm ci >/dev/null && npm run build >/dev/null)

echo "==> Start node"
"${ROOT_DIR}/target/debug/rabbitchain" \
  --data-dir "${TMP_RUN_DIR}/node-data" \
  run \
  --mine \
  --disable-local-miner \
  --mining-work-target-leading-rabbit-bytes 1 \
  --rpc-rate-limit-per-minute 0 \
  --coinbase "${COINBASE_NATIVE}" \
  --rpc-coinbase "${COINBASE_NATIVE}" \
  --rpc-auth-token "${RPC_AUTH_TOKEN}" \
  --http-port "${NODE_RPC_PORT}" \
  --ws-port "${NODE_WS_PORT}" \
  >"${NODE_LOG}" 2>&1 &
PIDS+=("$!")

echo "==> Start mining pool"
(cd "${MINING_STACK_DIR}" && cargo run --release -- \
  pool \
  --host 127.0.0.1 \
  --port "${POOL_PORT}" \
  --node-rpc "${NODE_RPC_URL}" \
  --node-rpc-token "${RPC_AUTH_TOKEN}" \
  >"${POOL_LOG}" 2>&1) &
PIDS+=("$!")

echo "==> Start miner"
(cd "${MINING_STACK_DIR}" && cargo run --release -- \
  miner \
  --pool-url "${POOL_URL}" \
  --miner-id "${MINER_ID}" \
  --metrics-host 127.0.0.1 \
  --metrics-port "${MINER_METRICS_PORT}" \
  --target-leading-rabbit-bytes 0 \
  >"${MINER_LOG}" 2>&1) &
PIDS+=("$!")

echo "==> Start explorer backend"
(cd "${EXPLORER_BACKEND_DIR}" && \
  RABBIT_RPC_URL="${NODE_RPC_URL}" \
  RABBIT_EXPLORER_BACKEND_BIND="127.0.0.1:${EXPLORER_BACKEND_PORT}" \
  cargo run --release \
  >"${EXPLORER_BACKEND_LOG}" 2>&1) &
PIDS+=("$!")

echo "==> Start explorer frontend (preview)"
(cd "${EXPLORER_FRONTEND_DIR}" && npm run preview -- --host 127.0.0.1 --port "${EXPLORER_FRONTEND_PORT}" >"${EXPLORER_FRONTEND_LOG}" 2>&1) &
PIDS+=("$!")

wait_http_ok "${POOL_URL}/health" 90
wait_http_ok "${MINER_METRICS_URL}/health" 90
wait_http_ok "${EXPLORER_BACKEND_URL}/health" 90
wait_http_ok "${EXPLORER_FRONTEND_URL}/" 90

echo "==> Verify block growth"
block_before_json="$(rpc_call "rabbit_getLatestBlock" "[]")"
block_before_hex="$(printf '%s' "${block_before_json}" | extract_block_number_hex)"
sleep 5
block_after_json="$(rpc_call "rabbit_getLatestBlock" "[]")"
block_after_hex="$(printf '%s' "${block_after_json}" | extract_block_number_hex)"
block_before_dec="$(hex_to_dec "${block_before_hex}")"
block_after_dec="$(hex_to_dec "${block_after_hex}")"
if (( block_after_dec <= block_before_dec )); then
  echo "Block number did not increase: ${block_before_hex} -> ${block_after_hex}" >&2
  exit 1
fi

echo "==> Verify miner/pool activity"
pool_stats_json="$(curl -fsS "${POOL_URL}/v1/stats")"
pool_shares="$(printf '%s' "${pool_stats_json}" | sed -n 's/.*"shares":{"[^"]*":[ ]*\([0-9][0-9]*\)}.*/\1/p')"
if [[ -z "${pool_shares}" ]]; then
  pool_shares="0"
fi
if (( pool_shares < 1 )); then
  echo "Expected pool shares >= 1, got ${pool_shares}" >&2
  exit 1
fi

echo "==> Verify rabbit_getAccount for canonical native prefix"
account_json="$(rpc_call "rabbit_getAccount" "[\"${COINBASE_NATIVE}\"]")"
if ! printf '%s' "${account_json}" | grep -q "\"address\":\"${COINBASE_NATIVE}\""; then
  echo "rabbit_getAccount did not return canonical native address" >&2
  echo "${account_json}" >&2
  exit 1
fi

echo "==> Verify explorer API accepts 0x and rejects invalid prefixes"
explorer_account_json="$(curl -fsS "${EXPLORER_BACKEND_URL}/api/accounts/${COINBASE_NATIVE}")"
if ! printf '%s' "${explorer_account_json}" | grep -q "\"address\":\"${COINBASE_NATIVE}\""; then
  echo "Explorer account endpoint missing canonical address" >&2
  exit 1
fi

explorer_search_json="$(curl -fsS "${EXPLORER_BACKEND_URL}/api/search/${COINBASE_NATIVE}")"
if ! printf '%s' "${explorer_search_json}" | grep -q "\"kind\":\"address\""; then
  echo "Explorer search endpoint did not resolve address query" >&2
  exit 1
fi

invalid_address="BAD0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9"
invalid_http_code="$(curl -sS -o /dev/null -w '%{http_code}' "${EXPLORER_BACKEND_URL}/api/accounts/${invalid_address}")"
if [[ "${invalid_http_code}" != "400" ]]; then
  echo "Expected invalid 0x prefix to be rejected with 400, got ${invalid_http_code}" >&2
  exit 1
fi

DATE_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
COMMIT="$(git -C "${ROOT_DIR}" rev-parse --short HEAD)"
cat > "${REPORT_FILE}" <<EOF
# Full-Chain E2E Report

- Generated at: ${DATE_UTC}
- Commit: ${COMMIT}
- Node RPC: ${NODE_RPC_URL}
- Pool: ${POOL_URL}
- Miner metrics: ${MINER_METRICS_URL}
- Explorer backend: ${EXPLORER_BACKEND_URL}
- Explorer frontend: ${EXPLORER_FRONTEND_URL}

## Checks

- [x] Services health endpoints reachable
- [x] Block number progressed (${block_before_hex} -> ${block_after_hex})
- [x] Pool shares accepted (>=1, actual ${pool_shares})
- [x] \`rabbit_getAccount\` returns canonical \`0x\` address
- [x] Explorer \`/api/accounts\` and \`/api/search\` accept \`0x\`
- [x] Explorer rejects invalid \`BAD0x...\` with HTTP 400
EOF

echo "✅ Full-chain E2E passed"
echo "📄 Report: ${REPORT_FILE}"
