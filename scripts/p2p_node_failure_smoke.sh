#!/usr/bin/env bash
# Node-failure isolation smoke:
# - Start a 3-node local P2P network.
# - Kill one follower and verify the bootnode + remaining follower stay healthy.
# - Kill the bootnode and verify the remaining follower still serves RPC.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_PATH="${ROOT_DIR}/target/debug/rabbitchain"
REPORT_DIR="${ROOT_DIR}/artifacts/p2p-node-failure"
LOG_DIR="${REPORT_DIR}/logs"
REPORT_FILE="${REPORT_DIR}/report.md"
mkdir -p "${REPORT_DIR}"
TMP_RUN_DIR="$(mktemp -d "${REPORT_DIR}/run.XXXXXX")"

RPC1="${RPC1:-28645}"
RPC2="${RPC2:-28655}"
RPC3="${RPC3:-28665}"
WS1="${WS1:-28646}"
WS2="${WS2:-28656}"
WS3="${WS3:-28666}"
P2P1="${P2P1:-41331}"
P2P2="${P2P2:-41332}"
P2P3="${P2P3:-41333}"

PIDS=()
NODE1_PID=""
NODE2_PID=""
NODE3_PID=""

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "${pid}" >/dev/null 2>&1 || true
  done
  wait >/dev/null 2>&1 || true
}
trap cleanup EXIT

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing command: $1" >&2
    exit 1
  }
}

assert_port_free() {
  local port="$1"
  if ss -ltn | grep -q ":${port}\\b"; then
    echo "Port ${port} is already in use" >&2
    exit 1
  fi
}

rpc_call() {
  local rpc_port="$1"
  local method="$2"
  local params_json="${3:-[]}"
  curl -fsS \
    -H 'content-type: application/json' \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params_json}}" \
    "http://127.0.0.1:${rpc_port}"
}

extract_result_hex() {
  sed -n 's/.*"result":"\([^"]*\)".*/\1/p'
}

hex_to_dec() {
  local hex="${1#0x}"
  if [[ -z "${hex}" ]]; then
    echo "0"
    return
  fi
  printf '%d' "$((16#${hex}))"
}

peer_count() {
  local rpc_port="$1"
  local json
  json="$(rpc_call "${rpc_port}" net_peerCount '[]')"
  hex_to_dec "$(printf '%s' "${json}" | extract_result_hex)"
}

wait_rpc_ready() {
  local rpc_port="$1"
  local timeout_secs="${2:-60}"
  local i=0
  while (( i < timeout_secs )); do
    if rpc_call "${rpc_port}" rabbit_clientVersion '[]' >/dev/null 2>&1; then
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  echo "Timeout waiting for RPC on :${rpc_port}" >&2
  return 1
}

wait_peer_count_at_least() {
  local rpc_port="$1"
  local min_peers="$2"
  local timeout_secs="${3:-60}"
  local i=0
  while (( i < timeout_secs )); do
    local dec
    dec="$(peer_count "${rpc_port}")"
    if (( dec >= min_peers )); then
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  echo "Timeout waiting net_peerCount >= ${min_peers} on :${rpc_port}" >&2
  return 1
}

assert_rpc_alive() {
  local rpc_port="$1"
  rpc_call "${rpc_port}" rabbit_clientVersion '[]' >/dev/null
  rpc_call "${rpc_port}" net_version '[]' >/dev/null
  rpc_call "${rpc_port}" rabbit_peers '[]' >/dev/null
}

assert_rpc_down() {
  local rpc_port="$1"
  if rpc_call "${rpc_port}" rabbit_clientVersion '[]' >/dev/null 2>&1; then
    echo "Expected RPC on :${rpc_port} to be down" >&2
    exit 1
  fi
}

kill_node() {
  local pid="$1"
  local label="$2"
  kill "${pid}" >/dev/null 2>&1 || true
  wait "${pid}" >/dev/null 2>&1 || true
  if kill -0 "${pid}" >/dev/null 2>&1; then
    echo "Failed to stop ${label} pid=${pid}" >&2
    exit 1
  fi
}

require_cmd cargo
require_cmd curl
require_cmd sed
require_cmd ss

mkdir -p "${LOG_DIR}" "${ROOT_DIR}/artifacts"

for port in "${RPC1}" "${RPC2}" "${RPC3}" "${WS1}" "${WS2}" "${WS3}" "${P2P1}" "${P2P2}" "${P2P3}"; do
  assert_port_free "${port}"
done

echo "==> Build rabbitcli"
cargo build -p rabbitcli >/dev/null

BOOTNODE_1="enode://bootnode-1@127.0.0.1:${P2P1}"

echo "==> Start node-1 bootnode"
"${BIN_PATH}" \
  --data-dir "${TMP_RUN_DIR}/node1" \
  run \
  --http-port "${RPC1}" \
  --ws-port "${WS1}" \
  --p2p-listen-addr 127.0.0.1 \
  --p2p-listen-port "${P2P1}" \
  --disable-discovery \
  --disable-sync \
  >"${LOG_DIR}/node1.log" 2>&1 &
NODE1_PID="$!"
PIDS+=("${NODE1_PID}")

echo "==> Start node-2 follower"
"${BIN_PATH}" \
  --data-dir "${TMP_RUN_DIR}/node2" \
  run \
  --http-port "${RPC2}" \
  --ws-port "${WS2}" \
  --p2p-listen-addr 127.0.0.1 \
  --p2p-listen-port "${P2P2}" \
  --bootnode "${BOOTNODE_1}" \
  --disable-discovery \
  --disable-sync \
  >"${LOG_DIR}/node2.log" 2>&1 &
NODE2_PID="$!"
PIDS+=("${NODE2_PID}")

echo "==> Start node-3 follower"
"${BIN_PATH}" \
  --data-dir "${TMP_RUN_DIR}/node3" \
  run \
  --http-port "${RPC3}" \
  --ws-port "${WS3}" \
  --p2p-listen-addr 127.0.0.1 \
  --p2p-listen-port "${P2P3}" \
  --bootnode "${BOOTNODE_1}" \
  --disable-discovery \
  --disable-sync \
  >"${LOG_DIR}/node3.log" 2>&1 &
NODE3_PID="$!"
PIDS+=("${NODE3_PID}")

wait_rpc_ready "${RPC1}" 90
wait_rpc_ready "${RPC2}" 90
wait_rpc_ready "${RPC3}" 90

echo "==> Wait initial convergence"
wait_peer_count_at_least "${RPC1}" 2 90
wait_peer_count_at_least "${RPC2}" 1 90
wait_peer_count_at_least "${RPC3}" 1 90

initial_node1_peers="$(peer_count "${RPC1}")"
initial_node2_peers="$(peer_count "${RPC2}")"
initial_node3_peers="$(peer_count "${RPC3}")"

echo "==> Kill node-3 and verify node-1/node-2 stay healthy"
kill_node "${NODE3_PID}" "node-3"
sleep 3
assert_rpc_down "${RPC3}"
assert_rpc_alive "${RPC1}"
assert_rpc_alive "${RPC2}"
node1_after_leaf_failure="$(peer_count "${RPC1}")"
node2_after_leaf_failure="$(peer_count "${RPC2}")"
if (( node1_after_leaf_failure < 1 || node2_after_leaf_failure < 1 )); then
  echo "Remaining nodes lost all peers after node-3 failure: node1=${node1_after_leaf_failure}, node2=${node2_after_leaf_failure}" >&2
  exit 1
fi

echo "==> Kill bootnode and verify remaining follower still serves RPC"
kill_node "${NODE1_PID}" "node-1"
sleep 3
assert_rpc_down "${RPC1}"
assert_rpc_alive "${RPC2}"
node2_after_bootnode_failure="$(peer_count "${RPC2}")"

DATE_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
COMMIT="$(git -C "${ROOT_DIR}" rev-parse --short HEAD)"
cat > "${REPORT_FILE}" <<EOF
# P2P Node Failure Smoke Report

- Generated at: ${DATE_UTC}
- Commit: ${COMMIT}
- node-1 RPC: http://127.0.0.1:${RPC1}
- node-2 RPC: http://127.0.0.1:${RPC2}
- node-3 RPC: http://127.0.0.1:${RPC3}

## Checks

- [x] 3-node network converged before failure.
- [x] Killing follower node-3 did not crash node-1 or node-2.
- [x] node-1 and node-2 RPC stayed healthy after node-3 failure.
- [x] node-1 and node-2 retained at least one peer after node-3 failure.
- [x] Killing bootnode node-1 did not crash remaining node-2.
- [x] node-2 RPC stayed healthy after bootnode failure.

## Peer Counts

- initial: node1=${initial_node1_peers}, node2=${initial_node2_peers}, node3=${initial_node3_peers}
- after node-3 failure: node1=${node1_after_leaf_failure}, node2=${node2_after_leaf_failure}
- after bootnode failure: node2=${node2_after_bootnode_failure}

## Logs

- ${LOG_DIR}/node1.log
- ${LOG_DIR}/node2.log
- ${LOG_DIR}/node3.log
EOF

echo "✅ P2P node failure smoke passed"
echo "📄 Report: ${REPORT_FILE}"
