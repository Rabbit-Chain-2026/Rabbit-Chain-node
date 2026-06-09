#!/usr/bin/env bash
# 3-node P2P convergence smoke:
# - node-2 / node-3 dial node-1 as bootnode
# - verify net_peerCount converges
# - verify rabbit_peers returns peer metadata

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN_PATH="${ROOT_DIR}/target/debug/rabbitchain"
LOG_DIR="${ROOT_DIR}/artifacts/p2p-3node-logs"
TMP_RUN_DIR="$(mktemp -d "${ROOT_DIR}/artifacts/p2p-3node-run.XXXXXX")"

RPC1="${RPC1:-18645}"
RPC2="${RPC2:-18655}"
RPC3="${RPC3:-18665}"
WS1="${WS1:-18646}"
WS2="${WS2:-18656}"
WS3="${WS3:-18666}"
P2P1="${P2P1:-31331}"
P2P2="${P2P2:-31332}"
P2P3="${P2P3:-31333}"

PIDS=()

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
  local params_json="$3"
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

wait_rpc_ready() {
  local rpc_port="$1"
  local timeout_secs="${2:-60}"
  local i=0
  while (( i < timeout_secs )); do
    if rpc_call "${rpc_port}" "rabbit_clientVersion" "[]" >/dev/null 2>&1; then
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
    local json
    json="$(rpc_call "${rpc_port}" "net_peerCount" "[]")"
    local hex
    hex="$(printf '%s' "${json}" | extract_result_hex)"
    local dec
    dec="$(hex_to_dec "${hex}")"
    if (( dec >= min_peers )); then
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  echo "Timeout waiting net_peerCount >= ${min_peers} on :${rpc_port}" >&2
  return 1
}

require_cmd cargo
require_cmd curl
require_cmd sed
require_cmd ss
mkdir -p "${LOG_DIR}" "${ROOT_DIR}/artifacts"

assert_port_free "${RPC1}"
assert_port_free "${RPC2}"
assert_port_free "${RPC3}"
assert_port_free "${WS1}"
assert_port_free "${WS2}"
assert_port_free "${WS3}"
assert_port_free "${P2P1}"
assert_port_free "${P2P2}"
assert_port_free "${P2P3}"

echo "==> Build rabbitcli"
cargo build -p rabbitcli >/dev/null

BOOTNODE_1="enode://bootnode-1@127.0.0.1:${P2P1}"

echo "==> Start node-1 (bootnode)"
"${BIN_PATH}" \
  --data-dir "${TMP_RUN_DIR}/node1" \
  run \
  --http-port "${RPC1}" \
  --ws-port "${WS1}" \
  --p2p-listen-addr "127.0.0.1" \
  --p2p-listen-port "${P2P1}" \
  --disable-discovery \
  --disable-sync \
  >"${LOG_DIR}/node1.log" 2>&1 &
PIDS+=("$!")

echo "==> Start node-2"
"${BIN_PATH}" \
  --data-dir "${TMP_RUN_DIR}/node2" \
  run \
  --http-port "${RPC2}" \
  --ws-port "${WS2}" \
  --p2p-listen-addr "127.0.0.1" \
  --p2p-listen-port "${P2P2}" \
  --bootnode "${BOOTNODE_1}" \
  --disable-discovery \
  --disable-sync \
  >"${LOG_DIR}/node2.log" 2>&1 &
PIDS+=("$!")

echo "==> Start node-3"
"${BIN_PATH}" \
  --data-dir "${TMP_RUN_DIR}/node3" \
  run \
  --http-port "${RPC3}" \
  --ws-port "${WS3}" \
  --p2p-listen-addr "127.0.0.1" \
  --p2p-listen-port "${P2P3}" \
  --bootnode "${BOOTNODE_1}" \
  --disable-discovery \
  --disable-sync \
  >"${LOG_DIR}/node3.log" 2>&1 &
PIDS+=("$!")

wait_rpc_ready "${RPC1}" 90
wait_rpc_ready "${RPC2}" 90
wait_rpc_ready "${RPC3}" 90

echo "==> Wait peer convergence"
wait_peer_count_at_least "${RPC1}" 2 90
wait_peer_count_at_least "${RPC2}" 1 90
wait_peer_count_at_least "${RPC3}" 1 90

echo "==> Verify rabbit_peers"
network_version_json="$(rpc_call "${RPC1}" "net_version" "[]")"
expected_network_id="$(printf '%s' "${network_version_json}" | sed -n 's/.*"result":"\([^"]*\)".*/\1/p')"
if [[ -z "${expected_network_id}" ]]; then
  echo "Failed to parse net_version from node-1: ${network_version_json}" >&2
  exit 1
fi
node1_peers_json="$(rpc_call "${RPC1}" "rabbit_peers" "[]")"
node1_peer_items="$(printf '%s' "${node1_peers_json}" | grep -o '"peer_id"' | wc -l | tr -d ' ')"
if (( node1_peer_items < 2 )); then
  echo "node-1 rabbit_peers expected >=2 entries, got ${node1_peer_items}" >&2
  echo "${node1_peers_json}" >&2
  exit 1
fi
if ! printf '%s' "${node1_peers_json}" | grep -q "\"network_id\":${expected_network_id}"; then
  echo "node-1 rabbit_peers missing expected network_id=${expected_network_id}" >&2
  echo "${node1_peers_json}" >&2
  exit 1
fi

echo "✅ P2P 3-node smoke passed"
echo "   node-1 peers: ${node1_peer_items}"
echo "   logs: ${LOG_DIR}"
