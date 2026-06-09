#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKSPACE_DIR="${ROOT_DIR}/.."
MINING_STACK_DIR="${WORKSPACE_DIR}/rabbitchain-mining-stack"
EXPLORER_BACKEND_DIR="${WORKSPACE_DIR}/rabbitchain-explorer/backend"

COINBASE="${COINBASE:-0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9}"
RPC_AUTH_TOKEN="${RPC_AUTH_TOKEN:-mainnet-local-bringup-token}"
POOL_PORT="${POOL_PORT:-9332}"
MINER_METRICS_PORT="${MINER_METRICS_PORT:-9333}"
EXPLORER_BACKEND_PORT="${EXPLORER_BACKEND_PORT:-19080}"
MINER_ID="${MINER_ID:-miner-local-mainnet-1}"
BOOTNODE_ENODE_PLACEHOLDER="enode://BOOTNODE_PEER_ID@127.0.0.1:30303"

PIDS=()
LOG_DIR="${ROOT_DIR}/artifacts/mainnet-local-bringup"
RUN_STATE_DIR="${ROOT_DIR}/artifacts/mainnet-local-bringup-state"
mkdir -p "${LOG_DIR}"
mkdir -p "${RUN_STATE_DIR}"

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "${pid}" >/dev/null 2>&1 || true
  done
  wait >/dev/null 2>&1 || true
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing command: $1" >&2
    exit 1
  }
}

wait_http_ok() {
  local url="$1"
  local timeout_secs="${2:-30}"
  local i=0
  while (( i < timeout_secs )); do
    if curl -fsS "${url}" >/dev/null 2>&1; then
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  echo "timeout waiting for ${url}" >&2
  return 1
}

require_cmd cargo
require_cmd curl

cd "${ROOT_DIR}"
./scripts/mainnet.sh stop bootnode >/dev/null 2>&1 || true
./scripts/mainnet.sh stop follower >/dev/null 2>&1 || true
./scripts/mainnet.sh stop observer >/dev/null 2>&1 || true

echo "==> start bootnode"
./scripts/mainnet.sh start bootnode \
  --mine \
  --disable-local-miner \
  --coinbase "${COINBASE}" \
  --rpc-auth-token "${RPC_AUTH_TOKEN}" \
  --rpc-rate-limit-per-minute 0 \
  --p2p-listen-addr 127.0.0.1

BOOTNODE_ENODE="$(grep -m1 'bootnode enode hint:' /root/.rabbitchain/mainnet/bootnode/bootnode.log 2>/dev/null | sed 's/.*hint: //')"
if [[ -z "${BOOTNODE_ENODE}" ]]; then
  BOOTNODE_ENODE="${BOOTNODE_ENODE_PLACEHOLDER}"
fi

echo "==> start follower"
./scripts/mainnet.sh start follower \
  --bootnode "${BOOTNODE_ENODE}" \
  --p2p-listen-addr 127.0.0.1

echo "==> start observer"
./scripts/mainnet.sh start observer \
  --bootnode "${BOOTNODE_ENODE}" \
  --p2p-listen-addr 127.0.0.1

echo "==> start pool"
(cd "${MINING_STACK_DIR}" && cargo run --release -- \
  pool \
  --host 127.0.0.1 \
  --port "${POOL_PORT}" \
  --node-rpc "http://127.0.0.1:8545" \
  --node-rpc-token "${RPC_AUTH_TOKEN}" \
  > "${LOG_DIR}/pool.log" 2>&1) &
pool_pid=$!
PIDS+=("${pool_pid}")
echo "${pool_pid}" > "${RUN_STATE_DIR}/pool.pid"

echo "==> start miner"
(cd "${MINING_STACK_DIR}" && cargo run --release -- \
  miner \
  --pool-url "http://127.0.0.1:${POOL_PORT}" \
  --miner-id "${MINER_ID}" \
  --metrics-host 127.0.0.1 \
  --metrics-port "${MINER_METRICS_PORT}" \
  --target-leading-rabbit-bytes 0 \
  > "${LOG_DIR}/miner.log" 2>&1) &
miner_pid=$!
PIDS+=("${miner_pid}")
echo "${miner_pid}" > "${RUN_STATE_DIR}/miner.pid"

echo "==> start explorer backend"
(cd "${EXPLORER_BACKEND_DIR}" && \
  RABBIT_RPC_URL="http://127.0.0.1:39745" \
  RABBIT_EXPLORER_BACKEND_BIND="127.0.0.1:${EXPLORER_BACKEND_PORT}" \
  cargo run --release \
  > "${LOG_DIR}/explorer-backend.log" 2>&1) &
explorer_pid=$!
PIDS+=("${explorer_pid}")
echo "${explorer_pid}" > "${RUN_STATE_DIR}/explorer-backend.pid"

wait_http_ok "http://127.0.0.1:${POOL_PORT}/health" 60
wait_http_ok "http://127.0.0.1:${MINER_METRICS_PORT}/health" 60
wait_http_ok "http://127.0.0.1:${EXPLORER_BACKEND_PORT}/health" 60

echo
echo "mainnet local bring-up started"
echo "bootnode_enode=${BOOTNODE_ENODE}"
echo "bootnode_rpc=http://127.0.0.1:8545"
echo "follower_rpc=http://127.0.0.1:29645"
echo "observer_rpc=http://127.0.0.1:39745"
echo "pool_url=http://127.0.0.1:${POOL_PORT}"
echo "miner_metrics=http://127.0.0.1:${MINER_METRICS_PORT}"
echo "explorer_backend=http://127.0.0.1:${EXPLORER_BACKEND_PORT}"
echo
echo "next:"
echo "  ./scripts/mainnet.sh status bootnode"
echo "  ./scripts/mainnet.sh status follower"
echo "  ./scripts/mainnet.sh status observer"
echo "  scripts/node_sync_check.sh"
echo "  curl -fsS http://127.0.0.1:${POOL_PORT}/v1/stats"
echo "  curl -fsS http://127.0.0.1:${EXPLORER_BACKEND_PORT}/api/overview"
echo
echo "logs:"
echo "  ${LOG_DIR}/pool.log"
echo "  ${LOG_DIR}/miner.log"
echo "  ${LOG_DIR}/explorer-backend.log"

wait
