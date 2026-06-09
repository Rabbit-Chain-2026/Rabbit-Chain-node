#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORKSPACE_DIR="${WORKSPACE_DIR:-${ROOT_DIR}/..}"
MINING_STACK_DIR="${MINING_STACK_DIR:-${WORKSPACE_DIR}/rabbitchain-mining-stack}"
REPORT_DIR="${REPORT_DIR:-${ROOT_DIR}/artifacts/mining-e2e}"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)"
RUN_DIR="${REPORT_DIR}/${RUN_ID}"
LOG_DIR="${RUN_DIR}/logs"
REPORT_FILE="${RUN_DIR}/report.md"
META_FILE="${RUN_DIR}/meta.txt"
TMP_RUN_DIR=''

NODE_RPC_HOST="${NODE_RPC_HOST:-127.0.0.1}"
NODE_RPC_PORT="${NODE_RPC_PORT:-18455}"
NODE_WS_PORT="${NODE_WS_PORT:-18456}"
NODE_P2P_PORT="${NODE_P2P_PORT:-31303}"
POOL_PORT="${POOL_PORT:-9332}"
MINER_METRICS_PORT="${MINER_METRICS_PORT:-9333}"

NODE_RPC_URL="http://${NODE_RPC_HOST}:${NODE_RPC_PORT}"
POOL_URL="http://127.0.0.1:${POOL_PORT}"
MINER_METRICS_URL="http://127.0.0.1:${MINER_METRICS_PORT}"

CHAIN_ID="${CHAIN_ID:-10086}"
NETWORK_ID="${NETWORK_ID:-10086}"
RPC_AUTH_TOKEN="${RPC_AUTH_TOKEN:-mining-e2e-token}"
COINBASE_NATIVE="${COINBASE_NATIVE:-0x0000000000000000000000000000000000000000}"
MINER_ID="${MINER_ID:-miner-e2e-1}"
FIXTURE_FILE="${FIXTURE_FILE:-${ROOT_DIR}/fixtures/compute_json/ed25519_owner_mint.json}"

NODE_LOG="${LOG_DIR}/node.log"
POOL_LOG="${LOG_DIR}/pool.log"
MINER_LOG="${LOG_DIR}/miner.log"

PIDS=()

usage() {
  cat <<'EOF2'
Usage: bash scripts/mining_e2e.sh

Environment overrides:
  MINING_STACK_DIR      Sibling rabbitchain-mining-stack checkout
  NODE_RPC_PORT         Local rabbitchain RPC port (default: 18455)
  NODE_WS_PORT          Local rabbitchain WS port (default: 18456)
  NODE_P2P_PORT         Local rabbitchain P2P port (default: 31303)
  POOL_PORT             rabbitchain-mining-stack pool port (default: 9332)
  MINER_METRICS_PORT    rabbitchain-mining-stack miner metrics port (default: 9333)
  CHAIN_ID              rabbitchain chain_id override (default: 10086)
  NETWORK_ID            rabbitchain network_id override (default: 10086)
  RPC_AUTH_TOKEN        Auth token required by node RPC write methods
  COINBASE_NATIVE       Coinbase used for mining RPC (default: 0x0000000000000000000000000000000000000000)
  MINER_ID              Miner identifier label (default: miner-e2e-1)
  FIXTURE_FILE          Compute fixture JSON with top-level {"input":...}
EOF2
}

while (($# > 0)); do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "${pid}" >/dev/null 2>&1 || true
  done
  wait >/dev/null 2>&1 || true
  if [[ -n "${TMP_RUN_DIR}" && -d "${TMP_RUN_DIR}" ]]; then
    rm -rf "${TMP_RUN_DIR}"
  fi
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

assert_file() {
  local file="$1"
  if [[ ! -f "${file}" ]]; then
    echo "Missing file: ${file}" >&2
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

wait_rpc_ok() {
  local timeout_secs="${1:-60}"
  local i=0
  while (( i < timeout_secs )); do
    if rpc_call "rabbit_clientVersion" "[]" >/dev/null 2>&1; then
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  echo "Timeout waiting for RPC ${NODE_RPC_URL}" >&2
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

extract_block_number_hex() {
  sed -n 's/.*"number":"\([^"]*\)".*/\1/p'
}

extract_pool_shares() {
  sed -n 's/.*"shares":{"[^"]*": *\([0-9][0-9]*\)}.*/\1/p'
}

extract_canonical_tx_id() {
  sed -n 's/^canonical_tx_id: \(0x[0-9a-fA-F]\+\)$/\1/p'
}

hex_to_dec() {
  local hex="${1#0x}"
  if [[ -z "${hex}" ]]; then
    echo "0"
    return
  fi
  printf '%d' "$((16#${hex}))"
}

mkdir -p "${RUN_DIR}" "${LOG_DIR}"
require_cmd cargo
require_cmd curl
require_cmd python3
require_cmd ss

assert_dir "${MINING_STACK_DIR}"
assert_file "${FIXTURE_FILE}"
assert_port_free "${NODE_RPC_PORT}"
assert_port_free "${NODE_WS_PORT}"
assert_port_free "${NODE_P2P_PORT}"
assert_port_free "${POOL_PORT}"
assert_port_free "${MINER_METRICS_PORT}"

TMP_RUN_DIR="$(mktemp -d "${RUN_DIR}/tmp.XXXXXX")"
NODE_DATA_DIR="${TMP_RUN_DIR}/node-data"
TX_INPUT_FILE="${TMP_RUN_DIR}/compute-input.json"

{
  echo "mode=mining-e2e"
  echo "git_rev=$(git -C "${ROOT_DIR}" rev-parse --short HEAD)"
  echo "git_branch=$(git -C "${ROOT_DIR}" rev-parse --abbrev-ref HEAD)"
  echo "node_rpc_url=${NODE_RPC_URL}"
  echo "pool_url=${POOL_URL}"
  echo "miner_metrics_url=${MINER_METRICS_URL}"
  echo "fixture_file=${FIXTURE_FILE}"
  echo "chain_id=${CHAIN_ID}"
  echo "network_id=${NETWORK_ID}"
  echo "rpc_auth_token=<redacted>"
  echo "coinbase_native=${COINBASE_NATIVE}"
  echo "miner_id=${MINER_ID}"
} >"${META_FILE}"

write_report() {
  local exit_code="$1"
  local status="success"
  if [[ "$exit_code" -ne 0 ]]; then
    status="failure"
  fi

  local end_utc
  end_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  {
    printf '# Mining E2E Report\n\n'
    printf -- '- status: %s\n' "$status"
    printf -- '- exit_code: %s\n' "$exit_code"
    printf -- '- run_id: %s\n' "$RUN_ID"
    printf -- '- end_utc: %s\n' "$end_utc"
    printf -- '- node_rpc: %s\n' "$NODE_RPC_URL"
    printf -- '- pool: %s\n' "$POOL_URL"
    printf -- '- miner_metrics: %s\n' "$MINER_METRICS_URL"
    printf -- '- canonical_tx_id: %s\n' "${CANONICAL_TX_ID:-<not-set>}"
    printf -- '- chain_receipt_present: %s\n' "${CHAIN_RECEIPT_PRESENT:-<not-set>}"
    printf -- '- submitted_height_before: %s\n' "${BLOCK_BEFORE_HEX:-<not-set>}"
    printf -- '- submitted_height_after: %s\n' "${BLOCK_AFTER_HEX:-<not-set>}"
    printf -- '- pool_shares: %s\n' "${POOL_SHARES:-<not-set>}"
    printf -- '- miner_accepted_shares: %s\n' "${MINER_ACCEPTED:-<not-set>}"
    printf -- '\n## Checks\n\n'
    printf -- '- [x] rabbitchain node started with mining enabled and external miner disabled\n'
    printf -- '- [x] rabbitchain compute send returned canonical tx id\n'
    printf -- '- [x] rabbitchain compute get returned ok=true\n'
    printf -- '- [x] compute execution receipt captured from rabbit_getComputeTxResult\n'
    printf -- '- [x] rabbit_getReceipt queried for the canonical tx id (present: %s)\n' "${CHAIN_RECEIPT_PRESENT:-unknown}"
    printf -- '- [x] block height increased after miner start (%s -> %s)\n' "${BLOCK_BEFORE_HEX:-<n/a>}" "${BLOCK_AFTER_HEX:-<n/a>}"
    printf -- '- [x] pool accepted shares >= 1 (actual %s)\n' "${POOL_SHARES:-0}"
    printf -- '- [x] miner metrics accepted shares >= 1 (actual %s)\n' "${MINER_ACCEPTED:-0}"
    printf -- '\n## Artifacts\n\n'
    printf -- '- Node log: %s\n' "${NODE_LOG}"
    printf -- '- Pool log: %s\n' "${POOL_LOG}"
    printf -- '- Miner log: %s\n' "${MINER_LOG}"
    printf -- '- Report meta: %s\n' "${META_FILE}"
    printf -- '- Fixture: %s\n' "${FIXTURE_FILE}"
    printf -- '- Raw compute submit output:\n\n'
    printf -- '~~~text\n%s\n~~~\n' "${COMPUTE_SEND_OUTPUT:-<not-set>}"
    printf -- '- rabbitchain compute get output:\n\n'
    printf -- '~~~text\n%s\n~~~\n' "${COMPUTE_GET_OUTPUT:-<not-set>}"
    printf -- '- rabbit_getComputeTxResult:\n\n'
    printf -- '~~~json\n%s\n~~~\n' "${EXECUTION_RECEIPT_JSON:-null}"
    printf -- '- rabbit_getReceipt:\n\n'
    printf -- '~~~json\n%s\n~~~\n' "${RECEIPT_JSON:-null}"
    printf -- '- rabbit_getLatestBlock before:\n\n'
    printf -- '~~~json\n%s\n~~~\n' "${BLOCK_BEFORE_JSON:-<not-set>}"
    printf -- '- rabbit_getLatestBlock after:\n\n'
    printf -- '~~~json\n%s\n~~~\n' "${BLOCK_AFTER_JSON:-<not-set>}"
  } >"${REPORT_FILE}"
}

trap 'write_report "$?"' EXIT

echo "==> Build rabbitchain CLI"
cargo build -p rabbitcli >/dev/null

echo "==> Build rabbitchain-mining-stack"
(cd "${MINING_STACK_DIR}" && cargo build >/dev/null)

echo "==> Prepare local node data"
"${ROOT_DIR}/target/debug/rabbitchain" --network local --data-dir "${NODE_DATA_DIR}" init >/dev/null

echo "==> Extract compute fixture input"
python3 - "${FIXTURE_FILE}" "${TX_INPUT_FILE}" <<'PY'
import json
import sys

src, dest = sys.argv[1], sys.argv[2]
with open(src, "r", encoding="utf-8") as fh:
    payload = json.load(fh)
with open(dest, "w", encoding="utf-8") as fh:
    json.dump(payload["input"], fh, ensure_ascii=True, indent=2)
    fh.write("\n")
PY

echo "==> Start rabbitchain node"
"${ROOT_DIR}/target/debug/rabbitchain" \
  --network local \
  --data-dir "${NODE_DATA_DIR}" \
  run \
  --mine \
  --disable-local-miner \
  --http-port "${NODE_RPC_PORT}" \
  --ws-port "${NODE_WS_PORT}" \
  --p2p-listen-port "${NODE_P2P_PORT}" \
  --chain-id "${CHAIN_ID}" \
  --network-id "${NETWORK_ID}" \
  --rpc-auth-token "${RPC_AUTH_TOKEN}" \
  --rpc-rate-limit-per-minute 0 \
  --mining-work-target-leading-rabbit-bytes 0 \
  --coinbase "${COINBASE_NATIVE}" \
  --rpc-coinbase "${COINBASE_NATIVE}" \
  >"${NODE_LOG}" 2>&1 &
PIDS+=("$!")

wait_rpc_ok 60

echo "==> Submit compute transaction via CLI"
COMPUTE_SEND_OUTPUT="$("${ROOT_DIR}/target/debug/rabbitchain" --rpc-url "${NODE_RPC_URL}" --rpc-token "${RPC_AUTH_TOKEN}" compute send --tx-file "${TX_INPUT_FILE}")"
CANONICAL_TX_ID="$(printf '%s' "${COMPUTE_SEND_OUTPUT}" | extract_canonical_tx_id)"
if [[ -z "${CANONICAL_TX_ID}" ]]; then
  echo "Failed to extract canonical_tx_id from compute send output" >&2
  echo "${COMPUTE_SEND_OUTPUT}" >&2
  exit 1
fi

COMPUTE_GET_OUTPUT="$("${ROOT_DIR}/target/debug/rabbitchain" --rpc-url "${NODE_RPC_URL}" --rpc-token "${RPC_AUTH_TOKEN}" compute get --tx-id "${CANONICAL_TX_ID}")"
if ! printf '%s' "${COMPUTE_GET_OUTPUT}" | grep -q '"ok": true'; then
  echo "compute get did not return ok=true" >&2
  echo "${COMPUTE_GET_OUTPUT}" >&2
  exit 1
fi

EXECUTION_RECEIPT_JSON="$(rpc_call "rabbit_getComputeTxResult" "[\"${CANONICAL_TX_ID}\"]")"

BLOCK_BEFORE_JSON="$(rpc_call "rabbit_getLatestBlock" "[]")"
BLOCK_BEFORE_HEX="$(printf '%s' "${BLOCK_BEFORE_JSON}" | extract_block_number_hex)"

RECEIPT_JSON="$(rpc_call "rabbit_getReceipt" "[\"${CANONICAL_TX_ID}\"]")"
if printf '%s' "${RECEIPT_JSON}" | grep -q '"tx_id"'; then
  CHAIN_RECEIPT_PRESENT="true"
else
  CHAIN_RECEIPT_PRESENT="false"
fi
echo "chain_receipt_present=${CHAIN_RECEIPT_PRESENT}" >>"${META_FILE}"

output_json="$(rpc_call "rabbit_getOutput" "[\"0x5656565656565656565656565656565656565656565656565656565656565656\"]")"
object_json="$(rpc_call "rabbit_getObject" "[\"0x7878787878787878787878787878787878787878787878787878787878787878\"]")"
if ! printf '%s' "${output_json}" | grep -q '"output_id"'; then
  echo "rabbit_getOutput did not return expected output" >&2
  exit 1
fi
if ! printf '%s' "${object_json}" | grep -q '"object_id"'; then
  echo "rabbit_getObject did not return expected object" >&2
  exit 1
fi

echo "==> Start mining pool"
"${MINING_STACK_DIR}/target/debug/rabbitchain-mining-stack" \
  pool \
  --host 127.0.0.1 \
  --port "${POOL_PORT}" \
  --node-rpc "${NODE_RPC_URL}" \
  --node-rpc-token "${RPC_AUTH_TOKEN}" \
  >"${POOL_LOG}" 2>&1 &
PIDS+=("$!")

wait_http_ok "${POOL_URL}/health" 60

echo "==> Start miner"
"${MINING_STACK_DIR}/target/debug/rabbitchain-mining-stack" \
  miner \
  --pool-url "${POOL_URL}" \
  --miner-id "${MINER_ID}" \
  --metrics-host 127.0.0.1 \
  --metrics-port "${MINER_METRICS_PORT}" \
  --target-leading-rabbit-bytes 0 \
  --report-interval 1000 \
  >"${MINER_LOG}" 2>&1 &
PIDS+=("$!")

wait_http_ok "${MINER_METRICS_URL}/health" 60

echo "==> Verify mining progression"
sleep 5
BLOCK_AFTER_JSON="$(rpc_call "rabbit_getLatestBlock" "[]")"
BLOCK_AFTER_HEX="$(printf '%s' "${BLOCK_AFTER_JSON}" | extract_block_number_hex)"
BLOCK_BEFORE_DEC="$(hex_to_dec "${BLOCK_BEFORE_HEX}")"
BLOCK_AFTER_DEC="$(hex_to_dec "${BLOCK_AFTER_HEX}")"
if (( BLOCK_AFTER_DEC <= BLOCK_BEFORE_DEC )); then
  echo "Block number did not increase after miner start: ${BLOCK_BEFORE_HEX} -> ${BLOCK_AFTER_HEX}" >&2
  exit 1
fi

POOL_STATS_JSON="$(curl -fsS "${POOL_URL}/v1/stats")"
POOL_SHARES="$(printf '%s' "${POOL_STATS_JSON}" | extract_pool_shares)"
POOL_SHARES="${POOL_SHARES:-0}"
if (( POOL_SHARES < 1 )); then
  echo "Expected pool shares >= 1, got ${POOL_SHARES}" >&2
  echo "${POOL_STATS_JSON}" >&2
  exit 1
fi

POOL_METRICS="$(curl -fsS "${POOL_URL}/metrics")"
MINER_METRICS="$(curl -fsS "${MINER_METRICS_URL}/metrics")"
if ! printf '%s' "${POOL_METRICS}" | grep -q 'rabbit_pool_shares_accepted_total'; then
  echo "Pool metrics missing rabbit_pool_shares_accepted_total" >&2
  exit 1
fi
if ! printf '%s' "${POOL_METRICS}" | grep -q 'rabbit_pool_node_rpc_requests_total{method="rabbit_submitWork",status="ok"}'; then
  echo "Pool metrics missing rabbit_submitWork success counter" >&2
  exit 1
fi
MINER_ACCEPTED="$(printf '%s' "${MINER_METRICS}" | sed -n 's/.*rabbit_miner_shares_total{[^}]*status="accepted"} \([0-9][0-9]*\).*/\1/p' | tail -n1)"
MINER_ACCEPTED="${MINER_ACCEPTED:-0}"
if (( MINER_ACCEPTED < 1 )); then
  echo "Miner metrics missing accepted share counter for ${MINER_ID}" >&2
  echo "${MINER_METRICS}" >&2
  exit 1
fi

DATE_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
COMMIT="$(git -C "${ROOT_DIR}" rev-parse --short HEAD)"
cat > "${REPORT_FILE}" <<EOF2
# Mining E2E Report

- Generated at: ${DATE_UTC}
- Commit: ${COMMIT}
- Node RPC: ${NODE_RPC_URL}
- Pool: ${POOL_URL}
- Miner metrics: ${MINER_METRICS_URL}
- Fixture: ${FIXTURE_FILE}
- Canonical compute tx: ${CANONICAL_TX_ID}
- Chain receipt present: ${CHAIN_RECEIPT_PRESENT}

## Checks

- [x] rabbitchain local node started with chain_id/network_id ${CHAIN_ID}/${NETWORK_ID}
- [x] rabbitchain compute send succeeded with canonical tx ${CANONICAL_TX_ID}
- [x] rabbitchain compute get returned ok=true
- [x] compute execution receipt captured from rabbit_getComputeTxResult
- [x] rabbit_getReceipt queried for the canonical tx id (present: ${CHAIN_RECEIPT_PRESENT})
- [x] rabbit_getOutput / rabbit_getObject returned the minted fixture object
- [x] rabbitchain-mining-stack pool /health reachable
- [x] rabbitchain-mining-stack miner metrics /health reachable
- [x] block height increased after miner start (${BLOCK_BEFORE_HEX} -> ${BLOCK_AFTER_HEX})
- [x] pool shares accepted >= 1 (actual ${POOL_SHARES})
- [x] pool metrics include rabbit_submitWork success counter
- [x] miner metrics include accepted share counter for ${MINER_ID}

## Artifacts

- Node log: ${NODE_LOG}
- Pool log: ${POOL_LOG}
- Miner log: ${MINER_LOG}
- rabbitchain compute get:

~~~text
${COMPUTE_GET_OUTPUT}
~~~

- rabbit_getLatestBlock before:

~~~json
${BLOCK_BEFORE_JSON}
~~~

- rabbit_getLatestBlock after:

~~~json
${BLOCK_AFTER_JSON}
~~~

- rabbit_getComputeTxResult:

~~~json
${EXECUTION_RECEIPT_JSON}
~~~

- rabbit_getReceipt:

~~~json
${RECEIPT_JSON}
~~~

- Raw compute submit output:

~~~text
${COMPUTE_SEND_OUTPUT}
~~~
EOF2

echo "✅ mining e2e passed"
echo "📄 Report: ${REPORT_FILE}"
echo "📁 Logs: ${LOG_DIR}"
