#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="${DATA_DIR:-/root/.rabbitchain/mainnet/bootnode}"
WORKSPACE_DIR="${WORKSPACE_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
RABBITCHAIN_BIN="${RABBITCHAIN_BIN:-${WORKSPACE_DIR}/target/release/rabbitchain}"
ADDRESS_FILE="${ADDRESS_FILE:-${DATA_DIR}/coinbase-addresses-500.txt}"
RPC_TOKEN_FILE="${RPC_TOKEN_FILE:-${DATA_DIR}/rpc.token}"
HTTP_PORT="${HTTP_PORT:-8545}"
WS_PORT="${WS_PORT:-8546}"
P2P_LISTEN_ADDR="${P2P_LISTEN_ADDR:-0.0.0.0}"
P2P_LISTEN_PORT="${P2P_LISTEN_PORT:-30303}"
BOOTNODE="${BOOTNODE:-enode://node-be190477-3c8c-48aa-803d-d07372c4458e@192.168.1.188:33303}"
RPC_RATE_LIMIT="${RPC_RATE_LIMIT:-600}"
POLL_SECS="${POLL_SECS:-2}"
STATE_FILE="${STATE_FILE:-${DATA_DIR}/coinbase-rotate.state}"
SUPERVISOR_LOG_FILE="${SUPERVISOR_LOG_FILE:-${DATA_DIR}/coinbase-rotate-supervisor.log}"
MINER_LOG_FILE="${MINER_LOG_FILE:-/tmp/rabbitchain-mainnet-bootnode.log}"
MINER_PID_FILE="${MINER_PID_FILE:-${DATA_DIR}/bootnode.pid}"
SUPERVISOR_PID_FILE="${SUPERVISOR_PID_FILE:-${DATA_DIR}/coinbase-rotate-supervisor.pid}"
SYNC_BLOCKS_FILE="${SYNC_BLOCKS_FILE:-${DATA_DIR}/p2p-blocks.jsonl}"
MAX_ADDRESSES="${MAX_ADDRESSES:-0}"

mkdir -p "${DATA_DIR}"
: > "${SUPERVISOR_LOG_FILE}"
printf '%s\n' "$$" > "${SUPERVISOR_PID_FILE}"

log() {
  printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S %Z')" "$*" | tee -a "${SUPERVISOR_LOG_FILE}"
}

cleanup() {
  stop_miner || true
  rm -f "${SUPERVISOR_PID_FILE}"
}
trap cleanup EXIT INT TERM

rpc_token() {
  tr -d '\n' < "${RPC_TOKEN_FILE}"
}

rpc_call() {
  local method="$1"
  local params="${2:-[]}"
  curl -sS \
    -H 'content-type: application/json' \
    -H "authorization: Bearer $(rpc_token)" \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}" \
    "http://127.0.0.1:${HTTP_PORT}"
}

current_height() {
  rpc_call "rabbit_syncStatus" | jq -r '.result.local_head'
}

latest_coinbase() {
  rpc_call "rabbit_getLatestBlock" | jq -r '.result.coinbase'
}

persistent_height() {
  if [[ ! -f "${SYNC_BLOCKS_FILE}" ]]; then
    echo "0"
    return 0
  fi
  tail -n 1 "${SYNC_BLOCKS_FILE}" | jq -r '.number // 0'
}

current_height_or_persisted() {
  local height=""
  if height="$(current_height 2>/dev/null)"; then
    printf '%s\n' "${height}"
  else
    persistent_height
  fi
}

wait_for_rpc_ready() {
  local attempts="${1:-30}"
  local delay_secs="${2:-1}"
  local height=""
  local i=0
  while (( i < attempts )); do
    if height="$(current_height 2>/dev/null)"; then
      printf '%s\n' "${height}"
      return 0
    fi
    sleep "${delay_secs}"
    i=$((i + 1))
  done
  return 1
}

load_state_index() {
  if [[ -f "${STATE_FILE}" ]]; then
    awk -F= '$1 == "next_index" { print $2 }' "${STATE_FILE}"
  else
    echo "0"
  fi
}

save_state() {
  local next_index="$1"
  local mined_height="$2"
  local mined_coinbase="$3"
  cat > "${STATE_FILE}" <<EOF
next_index=${next_index}
last_mined_height=${mined_height}
last_mined_coinbase=${mined_coinbase}
updated_at=$(date '+%Y-%m-%d %H:%M:%S %Z')
EOF
}

stop_miner() {
  local pid=""
  if [[ -f "${MINER_PID_FILE}" ]]; then
    pid="$(tr -d '\n' < "${MINER_PID_FILE}")"
  fi

  if [[ -n "${pid}" ]] && kill -0 "${pid}" 2>/dev/null; then
    kill "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
  else
    pkill -f "rabbitchain -d ${DATA_DIR} --network mainnet run" 2>/dev/null || true
  fi
}

start_miner() {
  local coinbase="$1"
  stop_miner
  log "start miner coinbase=${coinbase}"
  nohup "${RABBITCHAIN_BIN}" \
    -d "${DATA_DIR}" \
    --network mainnet \
    run \
    --http-port "${HTTP_PORT}" \
    --ws-port "${WS_PORT}" \
    --p2p-listen-addr "${P2P_LISTEN_ADDR}" \
    --p2p-listen-port "${P2P_LISTEN_PORT}" \
    --bootnode "${BOOTNODE}" \
    --mine \
    --coinbase "${coinbase}" \
    --rpc-coinbase "${coinbase}" \
    --rpc-rate-limit-per-minute "${RPC_RATE_LIMIT}" \
    --rpc-auth-token "$(rpc_token)" \
    > "${MINER_LOG_FILE}" 2>&1 &
  local pid=$!
  printf '%s\n' "${pid}" > "${MINER_PID_FILE}"
  sleep 2
  if ! kill -0 "${pid}" 2>/dev/null; then
    log "miner exited immediately for coinbase=${coinbase}"
    tail -n 20 "${MINER_LOG_FILE}" | tee -a "${SUPERVISOR_LOG_FILE}" >/dev/null || true
    return 1
  fi
  if ! wait_for_rpc_ready 30 1 >/dev/null; then
    log "rpc did not become ready for coinbase=${coinbase}"
    tail -n 20 "${MINER_LOG_FILE}" | tee -a "${SUPERVISOR_LOG_FILE}" >/dev/null || true
    return 1
  fi
}

mapfile -t ALL_ADDRESSES < <(grep -v '^[[:space:]]*#' "${ADDRESS_FILE}" | awk 'NF { print $1 }')
if [[ "${#ALL_ADDRESSES[@]}" -eq 0 ]]; then
  log "no usable addresses in ${ADDRESS_FILE}"
  exit 1
fi

LIMIT="${#ALL_ADDRESSES[@]}"
if (( MAX_ADDRESSES > 0 && MAX_ADDRESSES < LIMIT )); then
  LIMIT="${MAX_ADDRESSES}"
fi

INDEX="$(load_state_index)"
if [[ -z "${INDEX}" ]]; then
  INDEX="0"
fi
if (( INDEX < 0 || INDEX >= LIMIT )); then
  log "state next_index=${INDEX} outside range 0..$((LIMIT - 1)); resetting to 0"
  INDEX="0"
fi

log "loaded ${#ALL_ADDRESSES[@]} addresses; processing up to ${LIMIT}; resume index=${INDEX}"

while (( INDEX < LIMIT )); do
  ADDRESS="${ALL_ADDRESSES[INDEX]}"
  START_HEIGHT="$(current_height_or_persisted)"
  log "index=${INDEX} address=${ADDRESS} start_height=${START_HEIGHT}"
  start_miner "${ADDRESS}"

  while true; do
    sleep "${POLL_SECS}"
    if [[ -f "${MINER_PID_FILE}" ]]; then
      PID="$(tr -d '\n' < "${MINER_PID_FILE}")"
      if [[ -n "${PID}" ]] && ! kill -0 "${PID}" 2>/dev/null; then
        log "miner pid=${PID} exited before block; restarting same address"
        start_miner "${ADDRESS}"
      fi
    fi

    HEIGHT="$(current_height 2>/dev/null || true)"
    if [[ -z "${HEIGHT}" ]]; then
      continue
    fi
    if (( HEIGHT > START_HEIGHT )); then
      COINBASE="$(latest_coinbase 2>/dev/null || true)"
      log "height advanced ${START_HEIGHT} -> ${HEIGHT}; latest coinbase=${COINBASE}"
      stop_miner
      INDEX=$((INDEX + 1))
      save_state "${INDEX}" "${HEIGHT}" "${COINBASE}"
      break
    fi
  done
done

log "coinbase rotation complete after ${LIMIT} addresses"
