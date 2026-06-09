#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

LOCAL_RABBITCHAIN_BIN="${LOCAL_RABBITCHAIN_BIN:-${ROOT_DIR}/target/release/rabbitchain}"
REMOTE_RABBITCHAIN_BIN="${REMOTE_RABBITCHAIN_BIN:-/root/rabbitchain_local.mainnet_sync}"

REMOTE_HOST="${REMOTE_HOST:-139.180.207.66}"
REMOTE_USER="${REMOTE_USER:-root}"
REMOTE_RPC_PORT="${REMOTE_RPC_PORT:-28545}"
REMOTE_WS_PORT="${REMOTE_WS_PORT:-28546}"
REMOTE_P2P_PORT="${REMOTE_P2P_PORT:-30303}"
SSH_KEY="${SSH_KEY:-/root/.ssh/agent_139_180_207_66}"
SSH_TIMEOUT_SECS="${SSH_TIMEOUT_SECS:-10}"

LOCAL_RPC_PORT="${LOCAL_RPC_PORT:-29645}"
LOCAL_WS_PORT="${LOCAL_WS_PORT:-29646}"
LOCAL_P2P_PORT="${LOCAL_P2P_PORT:-31303}"
OBSERVER_RPC_PORT="${OBSERVER_RPC_PORT:-39745}"
OBSERVER_WS_PORT="${OBSERVER_WS_PORT:-39746}"
OBSERVER_P2P_PORT="${OBSERVER_P2P_PORT:-32303}"

NETWORK_ID="${NETWORK_ID:-10086}"
CHAIN_ID="${CHAIN_ID:-10086}"
COINBASE="${COINBASE:-0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9}"

VERIFY_TIMEOUT_SECS="${VERIFY_TIMEOUT_SECS:-180}"
VERIFY_INTERVAL_SECS="${VERIFY_INTERVAL_SECS:-10}"
VERIFY_MIN_HEIGHT="${VERIFY_MIN_HEIGHT:-1}"
VERIFY_MAX_GAP="${VERIFY_MAX_GAP:-8}"
VERIFY_MAX_OBSERVER_GAP="${VERIFY_MAX_OBSERVER_GAP:-8}"

MONITOR_DURATION_SECS="${MONITOR_DURATION_SECS:-259200}"
MONITOR_INTERVAL_SECS="${MONITOR_INTERVAL_SECS:-60}"

log() {
  printf '[%s] %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*"
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 1
  }
}

ssh_cmd() {
  ssh -i "${SSH_KEY}" \
    -o StrictHostKeyChecking=no \
    -o BatchMode=yes \
    -o ConnectTimeout="${SSH_TIMEOUT_SECS}" \
    "${REMOTE_USER}@${REMOTE_HOST}" "$@"
}

rpc_local() {
  local method="$1"
  local params="${2:-[]}"
  curl -fsS --max-time 8 -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":${params},\"id\":1}" \
    "http://127.0.0.1:${LOCAL_RPC_PORT}"
}

rpc_remote() {
  local method="$1"
  local params="${2:-[]}"
  ssh_cmd "curl -fsS --max-time 8 -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":${params},\"id\":1}' http://127.0.0.1:${REMOTE_RPC_PORT}"
}

rpc_observer() {
  local method="$1"
  local params="${2:-[]}"
  curl -fsS --max-time 8 -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":${params},\"id\":1}" \
    "http://127.0.0.1:${OBSERVER_RPC_PORT}"
}

extract_result_hex() {
  sed -n 's/.*"result":"\([^"]*\)".*/\1/p'
}

extract_block_hex() {
  sed -n 's/.*"number":"\([^"]*\)".*/\1/p'
}

hex_to_dec() {
  local value="$1"
  local hex="${value#0x}"
  if [[ -z "${hex}" ]]; then
    printf '0\n'
  else
    printf '%d\n' "$((16#${hex}))"
  fi
}

wait_rpc_up() {
  local kind="$1"
  local timeout_secs="$2"
  local interval=2
  local elapsed=0
  while (( elapsed < timeout_secs )); do
    if [[ "${kind}" == "local" ]]; then
      if rpc_local net_version >/dev/null 2>&1; then
        return 0
      fi
    elif [[ "${kind}" == "observer" ]]; then
      if rpc_observer net_version >/dev/null 2>&1; then
        return 0
      fi
    else
      if rpc_remote net_version >/dev/null 2>&1; then
        return 0
      fi
    fi
    sleep "${interval}"
    elapsed=$((elapsed + interval))
  done
  return 1
}

wait_local_bootnode_enode() {
  local log_file="$1"
  local timeout_secs="$2"
  local interval=2
  local elapsed=0
  while (( elapsed < timeout_secs )); do
    local enode
    enode="$(grep -m1 'bootnode enode hint:' "${log_file}" 2>/dev/null | sed 's/.*hint: //')"
    if [[ -n "${enode}" ]]; then
      printf '%s\n' "${enode}"
      return 0
    fi
    sleep "${interval}"
    elapsed=$((elapsed + interval))
  done
  return 1
}

wait_remote_bootnode_enode() {
  local timeout_secs="$1"
  local interval=2
  local elapsed=0
  while (( elapsed < timeout_secs )); do
    local enode
    enode="$(ssh_cmd '
set -euo pipefail
if [ -f /root/works/Rabbit-Chain-node-public-soak/current.env ]; then
  . /root/works/Rabbit-Chain-node-public-soak/current.env
  if [ -n "${LOG_FILE:-}" ] && [ -f "${LOG_FILE}" ]; then
    grep -m1 "bootnode enode hint:" "${LOG_FILE}" 2>/dev/null | sed "s/.*hint: //"
  fi
fi
' 2>/dev/null || true)"
    if [[ -n "${enode}" ]]; then
      printf '%s\n' "${enode}"
      return 0
    fi
    sleep "${interval}"
    elapsed=$((elapsed + interval))
  done
  return 1
}

remote_bootnode_enode_to_public() {
  local enode="$1"
  printf '%s\n' "${enode/@0.0.0.0:/@${REMOTE_HOST}:}"
}

require_cmd curl
require_cmd ssh
require_cmd sed

if [[ ! -x "${LOCAL_RABBITCHAIN_BIN}" ]]; then
  echo "local rabbitchain binary not found: ${LOCAL_RABBITCHAIN_BIN}" >&2
  exit 1
fi
if [[ ! -f "${SSH_KEY}" ]]; then
  echo "ssh key not found: ${SSH_KEY}" >&2
  exit 1
fi

cd "${ROOT_DIR}"

log "停止公网监控"
./scripts/public_node_soak_monitor.sh stop || true

log "停止本地公网节点"
if [[ -f artifacts/public-node-soak/current.env ]]; then
  # shellcheck disable=SC1091
  source artifacts/public-node-soak/current.env || true
  if [[ -n "${NODE_PID:-}" ]] && kill -0 "${NODE_PID}" 2>/dev/null; then
    kill "${NODE_PID}" || true
    sleep 1
  fi
fi

log "停止 observer 节点"
if [[ -f artifacts/public-node-observer/current.env ]]; then
  # shellcheck disable=SC1091
  source artifacts/public-node-observer/current.env || true
  if [[ -n "${NODE_PID:-}" ]] && kill -0 "${NODE_PID}" 2>/dev/null; then
    kill "${NODE_PID}" || true
    sleep 1
  fi
fi

log "停止远端公网节点"
ssh_cmd '
set -euo pipefail
if [ -f /root/works/Rabbit-Chain-node-public-soak/current.env ]; then
  . /root/works/Rabbit-Chain-node-public-soak/current.env || true
  if [ -n "${NODE_PID:-}" ] && kill -0 "${NODE_PID}" 2>/dev/null; then
    kill "${NODE_PID}" || true
    sleep 1
  fi
fi
'

log "清空本地公网数据"
rm -rf artifacts/public-node-soak artifacts/public-node-observer artifacts/public-node-soak-monitor
mkdir -p artifacts/public-node-soak artifacts/public-node-soak-monitor
mkdir -p artifacts/public-node-observer

log "清空远端公网数据"
ssh_cmd '
set -euo pipefail
rm -rf /root/works/Rabbit-Chain-node-public-soak/[0-9]* \
       /root/works/Rabbit-Chain-node-public-soak/current.env \
       /root/works/Rabbit-Chain-node-public-soak/remote-node.pid
mkdir -p /root/works/Rabbit-Chain-node-public-soak
'

log "启动远端公网节点"
ssh_cmd "
set -euo pipefail
TS=\$(date -u +%Y%m%dT%H%M%SZ)
RUN_DIR=/root/works/Rabbit-Chain-node-public-soak/\${TS}
DATA_DIR=\${RUN_DIR}/remote-node-data
LOG_FILE=\${RUN_DIR}/remote-node.log
mkdir -p \"\${RUN_DIR}\"
if command -v setsid >/dev/null 2>&1; then
  setsid ${REMOTE_RABBITCHAIN_BIN} \\
    --data-dir \"\${DATA_DIR}\" \\
    run \\
    --mine \\
    --coinbase ${COINBASE} \\
    --http-port ${REMOTE_RPC_PORT} \\
    --ws-port ${REMOTE_WS_PORT} \\
    --p2p-listen-addr 0.0.0.0 \\
    --p2p-listen-port ${REMOTE_P2P_PORT} \\
    --chain-id ${CHAIN_ID} \\
    --network-id ${NETWORK_ID} \\
    --rpc-coinbase ${COINBASE} \\
    >\"\${LOG_FILE}\" 2>&1 < /dev/null &
else
  nohup ${REMOTE_RABBITCHAIN_BIN} \\
    --data-dir \"\${DATA_DIR}\" \\
    run \\
    --mine \\
    --coinbase ${COINBASE} \\
    --http-port ${REMOTE_RPC_PORT} \\
    --ws-port ${REMOTE_WS_PORT} \\
    --p2p-listen-addr 0.0.0.0 \\
    --p2p-listen-port ${REMOTE_P2P_PORT} \\
    --chain-id ${CHAIN_ID} \\
    --network-id ${NETWORK_ID} \\
    --rpc-coinbase ${COINBASE} \\
    >\"\${LOG_FILE}\" 2>&1 < /dev/null &
fi
NEW_PID=\$!
cat > /root/works/Rabbit-Chain-node-public-soak/current.env <<ENV
RUN_DIR=\${RUN_DIR}
DATA_DIR=\${DATA_DIR}
LOG_FILE=\${LOG_FILE}
NODE_PID=\${NEW_PID}
ENV
echo \"\${NEW_PID}\" > /root/works/Rabbit-Chain-node-public-soak/remote-node.pid
echo \"remote_pid=\${NEW_PID}\"
"

REMOTE_BOOTNODE_ENODE_RAW="$(wait_remote_bootnode_enode 25)" || {
  echo "remote bootnode enode hint did not become ready" >&2
  exit 1
}
REMOTE_BOOTNODE_ENODE="$(remote_bootnode_enode_to_public "${REMOTE_BOOTNODE_ENODE_RAW}")"
log "远端 bootnode enode=${REMOTE_BOOTNODE_ENODE}"

log "启动本地公网节点（follower，不挖矿）"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
LOCAL_RUN_DIR="${ROOT_DIR}/artifacts/public-node-soak/${TS}"
LOCAL_DATA_DIR="${LOCAL_RUN_DIR}/local-node-data"
LOCAL_LOG_FILE="${LOCAL_RUN_DIR}/local-node.log"
mkdir -p "${LOCAL_RUN_DIR}"
if command -v setsid >/dev/null 2>&1; then
  setsid "${LOCAL_RABBITCHAIN_BIN}" \
    --data-dir "${LOCAL_DATA_DIR}" \
    run \
    --coinbase "${COINBASE}" \
    --rpc-coinbase "${COINBASE}" \
    --http-port "${LOCAL_RPC_PORT}" \
    --ws-port "${LOCAL_WS_PORT}" \
    --p2p-listen-addr 0.0.0.0 \
    --p2p-listen-port "${LOCAL_P2P_PORT}" \
    --bootnode "${REMOTE_BOOTNODE_ENODE}" \
    --chain-id "${CHAIN_ID}" \
    --network-id "${NETWORK_ID}" \
    >"${LOCAL_LOG_FILE}" 2>&1 < /dev/null &
else
  nohup "${LOCAL_RABBITCHAIN_BIN}" \
    --data-dir "${LOCAL_DATA_DIR}" \
    run \
    --coinbase "${COINBASE}" \
    --rpc-coinbase "${COINBASE}" \
    --http-port "${LOCAL_RPC_PORT}" \
    --ws-port "${LOCAL_WS_PORT}" \
    --p2p-listen-addr 0.0.0.0 \
    --p2p-listen-port "${LOCAL_P2P_PORT}" \
    --bootnode "${REMOTE_BOOTNODE_ENODE}" \
    --chain-id "${CHAIN_ID}" \
    --network-id "${NETWORK_ID}" \
    >"${LOCAL_LOG_FILE}" 2>&1 < /dev/null &
fi
LOCAL_PID=$!
cat > artifacts/public-node-soak/current.env <<ENV
RUN_DIR=${LOCAL_RUN_DIR}
DATA_DIR=${LOCAL_DATA_DIR}
LOG_FILE=${LOCAL_LOG_FILE}
NODE_PID=${LOCAL_PID}
ENV
echo "${LOCAL_PID}" > artifacts/public-node-soak/local-node.pid

LOCAL_BOOTNODE_ENODE="$(wait_local_bootnode_enode "${LOCAL_LOG_FILE}" 25)" || {
  echo "local follower bootnode enode hint did not become ready" >&2
  exit 1
}
log "本地 follower enode=${LOCAL_BOOTNODE_ENODE}"

log "启动 observer 节点（follower，双 bootnode）"
OBSERVER_RUN_DIR="${ROOT_DIR}/artifacts/public-node-observer/${TS}"
OBSERVER_DATA_DIR="${OBSERVER_RUN_DIR}/observer-node-data"
OBSERVER_LOG_FILE="${OBSERVER_RUN_DIR}/observer-node.log"
mkdir -p "${OBSERVER_RUN_DIR}"
if command -v setsid >/dev/null 2>&1; then
  setsid "${LOCAL_RABBITCHAIN_BIN}" \
    --data-dir "${OBSERVER_DATA_DIR}" \
    run \
    --http-port "${OBSERVER_RPC_PORT}" \
    --ws-port "${OBSERVER_WS_PORT}" \
    --p2p-listen-addr 0.0.0.0 \
    --p2p-listen-port "${OBSERVER_P2P_PORT}" \
    --bootnode "${LOCAL_BOOTNODE_ENODE}" \
    --bootnode "${REMOTE_BOOTNODE_ENODE}" \
    --chain-id "${CHAIN_ID}" \
    --network-id "${NETWORK_ID}" \
    --rpc-coinbase "${COINBASE}" \
    >"${OBSERVER_LOG_FILE}" 2>&1 < /dev/null &
else
  nohup "${LOCAL_RABBITCHAIN_BIN}" \
    --data-dir "${OBSERVER_DATA_DIR}" \
    run \
    --http-port "${OBSERVER_RPC_PORT}" \
    --ws-port "${OBSERVER_WS_PORT}" \
    --p2p-listen-addr 0.0.0.0 \
    --p2p-listen-port "${OBSERVER_P2P_PORT}" \
    --bootnode "${LOCAL_BOOTNODE_ENODE}" \
    --bootnode "${REMOTE_BOOTNODE_ENODE}" \
    --chain-id "${CHAIN_ID}" \
    --network-id "${NETWORK_ID}" \
    --rpc-coinbase "${COINBASE}" \
    >"${OBSERVER_LOG_FILE}" 2>&1 < /dev/null &
fi
OBSERVER_PID=$!
cat > artifacts/public-node-observer/current.env <<ENV
RUN_DIR=${OBSERVER_RUN_DIR}
DATA_DIR=${OBSERVER_DATA_DIR}
LOG_FILE=${OBSERVER_LOG_FILE}
NODE_PID=${OBSERVER_PID}
ENV
echo "${OBSERVER_PID}" > artifacts/public-node-observer/observer-node.pid

if ! wait_rpc_up remote 25; then
  echo "remote rpc did not become ready" >&2
  exit 1
fi
if ! wait_rpc_up local 25; then
  echo "local rpc did not become ready" >&2
  exit 1
fi
if ! wait_rpc_up observer 25; then
  echo "observer rpc did not become ready" >&2
  exit 1
fi

# discover remote pid from remote env
REMOTE_PID="$(ssh_cmd 'set -euo pipefail; . /root/works/Rabbit-Chain-node-public-soak/current.env; printf "%s" "${NODE_PID}"')"

log "启动公网监控"
./scripts/public_node_soak_monitor.sh start \
  --duration-secs "${MONITOR_DURATION_SECS}" \
  --interval-secs "${MONITOR_INTERVAL_SECS}" \
  --local-rpc-url "http://127.0.0.1:${LOCAL_RPC_PORT}" \
  --remote-host "${REMOTE_HOST}" \
  --remote-user "${REMOTE_USER}" \
  --remote-rpc-port "${REMOTE_RPC_PORT}" \
  --ssh-key "${SSH_KEY}" \
  --local-node-pid "${LOCAL_PID}" \
  --remote-node-pid "${REMOTE_PID}" \
  --rpc-timeout-secs 8 \
  --ssh-timeout-secs "${SSH_TIMEOUT_SECS}"

log "验证公网节点高度增长与同步"
start_ts="$(date +%s)"
pass=0
local_remote_hash_ok=0
observer_remote_hash_ok=0
while true; do
  now_ts="$(date +%s)"
  if (( now_ts - start_ts > VERIFY_TIMEOUT_SECS )); then
    break
  fi

  local_peers_hex="$(rpc_local net_peerCount | extract_result_hex || true)"
  remote_peers_hex="$(rpc_remote net_peerCount | extract_result_hex || true)"
  observer_peers_hex="$(rpc_observer net_peerCount | extract_result_hex || true)"
  local_block_hex="$(rpc_local rabbit_getLatestBlock | extract_block_hex || true)"
  remote_block_hex="$(rpc_remote rabbit_getLatestBlock | extract_block_hex || true)"
  observer_block_hex="$(rpc_observer rabbit_getLatestBlock | extract_block_hex || true)"

  if [[ -n "${local_peers_hex}" && -n "${remote_peers_hex}" && -n "${observer_peers_hex}" && -n "${local_block_hex}" && -n "${remote_block_hex}" && -n "${observer_block_hex}" ]]; then
    local_peers_dec="$(hex_to_dec "${local_peers_hex}")"
    remote_peers_dec="$(hex_to_dec "${remote_peers_hex}")"
    observer_peers_dec="$(hex_to_dec "${observer_peers_hex}")"
    local_block_dec="$(hex_to_dec "${local_block_hex}")"
    remote_block_dec="$(hex_to_dec "${remote_block_hex}")"
    observer_block_dec="$(hex_to_dec "${observer_block_hex}")"

    local_gap=$(( local_block_dec - remote_block_dec ))
    if (( local_gap < 0 )); then local_gap=$(( -local_gap )); fi
    observer_gap=$(( observer_block_dec - remote_block_dec ))
    if (( observer_gap < 0 )); then observer_gap=$(( -observer_gap )); fi

    log "sample peers(local=${local_peers_dec},observer=${observer_peers_dec},remote=${remote_peers_dec}) blocks(local=${local_block_dec},observer=${observer_block_dec},remote=${remote_block_dec}) gap(local=${local_gap},observer=${observer_gap})"

    if (( local_peers_dec >= 1 && observer_peers_dec >= 1 && remote_peers_dec >= 1 && local_block_dec >= VERIFY_MIN_HEIGHT && observer_block_dec >= VERIFY_MIN_HEIGHT && remote_block_dec >= VERIFY_MIN_HEIGHT && local_gap <= VERIFY_MAX_GAP && observer_gap <= VERIFY_MAX_OBSERVER_GAP )); then
      local_hash="$(
        rpc_local rabbit_getBlockByNumber "[\"${local_block_hex}\"]" \
          | sed -n 's/.*"hash":"\([^"]*\)".*/\1/p'
      )"
      remote_hash_for_local="$(
        ssh_cmd "curl -fsS --max-time 8 -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"rabbit_getBlockByNumber\",\"params\":[\"${local_block_hex}\"],\"id\":1}' http://127.0.0.1:${REMOTE_RPC_PORT}" \
          | sed -n 's/.*"hash":"\([^"]*\)".*/\1/p'
      )"
      observer_hash="$(
        rpc_observer rabbit_getBlockByNumber "[\"${observer_block_hex}\"]" \
          | sed -n 's/.*"hash":"\([^"]*\)".*/\1/p'
      )"
      remote_hash_for_observer="$(
        ssh_cmd "curl -fsS --max-time 8 -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"rabbit_getBlockByNumber\",\"params\":[\"${observer_block_hex}\"],\"id\":1}' http://127.0.0.1:${REMOTE_RPC_PORT}" \
          | sed -n 's/.*"hash":"\([^"]*\)".*/\1/p'
      )"

      if [[ -n "${local_hash}" && "${local_hash}" == "${remote_hash_for_local}" ]]; then
        local_remote_hash_ok=1
      fi
      if [[ -n "${observer_hash}" && "${observer_hash}" == "${remote_hash_for_observer}" ]]; then
        observer_remote_hash_ok=1
      fi

      if (( local_remote_hash_ok == 1 && observer_remote_hash_ok == 1 )); then
        log "hash parity(local=${local_block_hex}, observer=${observer_block_hex}) verified"
        pass=1
        break
      fi
      log "hash parity not ready yet(local_ok=${local_remote_hash_ok}, observer_ok=${observer_remote_hash_ok})"
    fi
  fi

  sleep "${VERIFY_INTERVAL_SECS}"
done

if (( pass != 1 )); then
  echo "verification failed within timeout=${VERIFY_TIMEOUT_SECS}s" >&2
  exit 1
fi

log "重置与验证完成"
log "local_pid=${LOCAL_PID} observer_pid=${OBSERVER_PID} remote_pid=${REMOTE_PID}"
log "local_run_dir=${LOCAL_RUN_DIR}"
log "observer_run_dir=${OBSERVER_RUN_DIR}"
log "use scripts/mainnet_checklist.sh for periodic checks"
