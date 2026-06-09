#!/usr/bin/env bash
# Local smoke for rabbitchain CLI + compute RPC + rabbitchain-mining-stack pool/miner.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORKSPACE_DIR="${WORKSPACE_DIR:-${ROOT_DIR}/..}"
MINING_STACK_DIR="${MINING_STACK_DIR:-${WORKSPACE_DIR}/rabbitchain-mining-stack}"
REPORT_DIR="${ROOT_DIR}/artifacts/cli-mining-smoke"
LOG_DIR="${REPORT_DIR}/logs"
REPORT_FILE="${REPORT_DIR}/report.md"
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
RPC_AUTH_TOKEN="${RPC_AUTH_TOKEN:-cli-mining-smoke-token}"
COINBASE_NATIVE="${COINBASE_NATIVE:-0x0000000000000000000000000000000000000000}"
MINER_ID="${MINER_ID:-miner-smoke-1}"
FIXTURE_FILE="${FIXTURE_FILE:-${ROOT_DIR}/fixtures/compute_json/ed25519_owner_mint.json}"

NODE_LOG="${LOG_DIR}/node.log"
POOL_LOG="${LOG_DIR}/pool.log"
MINER_LOG="${LOG_DIR}/miner.log"

PIDS=()

usage() {
  cat <<'EOF'
Usage: bash scripts/cli_mining_smoke.sh

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
  MINER_ID              Miner identifier label (default: miner-smoke-1)
  FIXTURE_FILE          Compute fixture JSON with top-level {"input":...}
EOF
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
    if rpc_call "net_version" "[]" >/dev/null 2>&1; then
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

extract_result_string() {
  sed -n 's/.*"result":"\([^"]*\)".*/\1/p'
}

extract_block_number_hex() {
  sed -n 's/.*"number":"\([^"]*\)".*/\1/p'
}

extract_pool_shares() {
  sed -n 's/.*"shares":{"[^"]*":[ ]*\([0-9][0-9]*\)}.*/\1/p'
}

hex_to_dec() {
  local hex="${1#0x}"
  if [[ -z "${hex}" ]]; then
    echo "0"
    return
  fi
  printf '%d' "$((16#${hex}))"
}

mkdir -p "${REPORT_DIR}" "${LOG_DIR}"
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

TMP_RUN_DIR="$(mktemp -d "${REPORT_DIR}/run.XXXXXX")"
NODE_DATA_DIR="${TMP_RUN_DIR}/node-data"
TX_INPUT_FILE="${TMP_RUN_DIR}/compute-input.json"

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

echo "==> Verify block CLI and RPC"
latest_block_json="$("${ROOT_DIR}/target/debug/rabbitchain" --rpc-url "${NODE_RPC_URL}" --rpc-token "${RPC_AUTH_TOKEN}" block latest)"
genesis_block_json="$("${ROOT_DIR}/target/debug/rabbitchain" --rpc-url "${NODE_RPC_URL}" --rpc-token "${RPC_AUTH_TOKEN}" block get --number 0)"
net_version_json="$(rpc_call "net_version" "[]")"
get_work_before_json="$(rpc_call "rabbit_getWork" "[]")"

if ! printf '%s' "${latest_block_json}" | grep -q 'number": "0x0"'; then
  echo "Expected latest block to start at genesis" >&2
  echo "${latest_block_json}" >&2
  exit 1
fi
if ! printf '%s' "${genesis_block_json}" | grep -q 'number": "0x0"'; then
  echo "Expected block 0 lookup to return genesis" >&2
  echo "${genesis_block_json}" >&2
  exit 1
fi

net_version="$(printf '%s' "${net_version_json}" | extract_result_string)"
if [[ "${net_version}" != "${NETWORK_ID}" ]]; then
  echo "Unexpected net_version: ${net_version} (expected ${NETWORK_ID})" >&2
  exit 1
fi

echo "==> Submit compute transaction via CLI"
compute_send_output="$("${ROOT_DIR}/target/debug/rabbitchain" --rpc-url "${NODE_RPC_URL}" --rpc-token "${RPC_AUTH_TOKEN}" compute send --tx-file "${TX_INPUT_FILE}")"
canonical_tx_id="$(printf '%s' "${compute_send_output}" | sed -n 's/^canonical_tx_id: \(0x[0-9a-fA-F]\+\)$/\1/p')"
if [[ -z "${canonical_tx_id}" ]]; then
  echo "Failed to extract canonical_tx_id from compute send output" >&2
  echo "${compute_send_output}" >&2
  exit 1
fi

compute_get_output="$("${ROOT_DIR}/target/debug/rabbitchain" --rpc-url "${NODE_RPC_URL}" --rpc-token "${RPC_AUTH_TOKEN}" compute get --tx-id "${canonical_tx_id}")"
output_json="$(rpc_call "rabbit_getOutput" "[\"0x5656565656565656565656565656565656565656565656565656565656565656\"]")"
object_json="$(rpc_call "rabbit_getObject" "[\"0x7878787878787878787878787878787878787878787878787878787878787878\"]")"

if ! printf '%s' "${compute_get_output}" | grep -q '"ok": true'; then
  echo "Compute result did not return ok=true" >&2
  echo "${compute_get_output}" >&2
  exit 1
fi
if ! printf '%s' "${output_json}" | grep -q '"output_id":"0x5656565656565656565656565656565656565656565656565656565656565656"'; then
  echo "rabbit_getOutput did not return expected output" >&2
  echo "${output_json}" >&2
  exit 1
fi
if ! printf '%s' "${object_json}" | grep -q '"object_id":"0x7878787878787878787878787878787878787878787878787878787878787878"'; then
  echo "rabbit_getObject did not return expected object" >&2
  echo "${object_json}" >&2
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
block_before_rpc="$(rpc_call "rabbit_getLatestBlock" "[]")"
block_before_hex="$(printf '%s' "${block_before_rpc}" | extract_block_number_hex)"
sleep 5
block_after_rpc="$(rpc_call "rabbit_getLatestBlock" "[]")"
block_after_hex="$(printf '%s' "${block_after_rpc}" | extract_block_number_hex)"
block_before_dec="$(hex_to_dec "${block_before_hex}")"
block_after_dec="$(hex_to_dec "${block_after_hex}")"
if (( block_after_dec <= block_before_dec )); then
  echo "Block number did not increase after miner start: ${block_before_hex} -> ${block_after_hex}" >&2
  exit 1
fi

pool_stats_json="$(curl -fsS "${POOL_URL}/v1/stats")"
pool_shares="$(printf '%s' "${pool_stats_json}" | extract_pool_shares)"
pool_shares="${pool_shares:-0}"
if (( pool_shares < 1 )); then
  echo "Expected pool shares >= 1, got ${pool_shares}" >&2
  echo "${pool_stats_json}" >&2
  exit 1
fi

pool_metrics="$(curl -fsS "${POOL_URL}/metrics")"
miner_metrics="$(curl -fsS "${MINER_METRICS_URL}/metrics")"
get_work_after_json="$(rpc_call "rabbit_getWork" "[]")"

if ! printf '%s' "${pool_metrics}" | grep -q 'rabbit_pool_shares_accepted_total'; then
  echo "Pool metrics missing rabbit_pool_shares_accepted_total" >&2
  exit 1
fi
if ! printf '%s' "${pool_metrics}" | grep -q 'rabbit_pool_node_rpc_requests_total{method="rabbit_submitWork",status="ok"}'; then
  echo "Pool metrics missing rabbit_submitWork success counter" >&2
  exit 1
fi
if ! printf '%s' "${miner_metrics}" | grep -q 'rabbit_miner_shares_total{miner="'"${MINER_ID}"'",status="accepted"}'; then
  echo "Miner metrics missing accepted share counter for ${MINER_ID}" >&2
  exit 1
fi

DATE_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
COMMIT="$(git -C "${ROOT_DIR}" rev-parse --short HEAD)"
cat > "${REPORT_FILE}" <<EOF
# CLI + Mining Smoke Report

- Generated at: ${DATE_UTC}
- Commit: ${COMMIT}
- Node RPC: ${NODE_RPC_URL}
- Pool: ${POOL_URL}
- Miner metrics: ${MINER_METRICS_URL}
- Fixture: ${FIXTURE_FILE}
- Canonical compute tx: ${canonical_tx_id}

## Checks

- [x] rabbitchain local node started with chain_id/network_id ${CHAIN_ID}/${NETWORK_ID}
- [x] \`rabbitchain block latest\` returned genesis before external miner start
- [x] \`rabbitchain block get --number 0\` returned genesis
- [x] \`net_version\` returned ${net_version}
- [x] \`rabbit_getWork\` returned a mining job before pool start
- [x] \`rabbitchain compute send\` succeeded with canonical tx ${canonical_tx_id}
- [x] \`rabbitchain compute get\` returned ok=true
- [x] \`rabbit_getOutput\` / \`rabbit_getObject\` returned the minted fixture object
- [x] rabbitchain-mining-stack pool /health reachable
- [x] rabbitchain-mining-stack miner metrics /health reachable
- [x] block height increased after miner start (${block_before_hex} -> ${block_after_hex})
- [x] pool shares accepted >= 1 (actual ${pool_shares})
- [x] pool metrics include \`rabbit_submitWork\` success counter
- [x] miner metrics include accepted share counter for ${MINER_ID}

## Artifacts

- Node log: ${NODE_LOG}
- Pool log: ${POOL_LOG}
- Miner log: ${MINER_LOG}
- rabbit_getWork before pool:

\`\`\`json
${get_work_before_json}
\`\`\`

- rabbit_getWork after mining started:

\`\`\`json
${get_work_after_json}
\`\`\`
EOF

echo "✅ cli + mining smoke passed"
echo "📄 Report: ${REPORT_FILE}"
echo "📁 Logs: ${LOG_DIR}"
