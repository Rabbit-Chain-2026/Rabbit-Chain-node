#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

MINING_RPC_URL="${MINING_RPC_URL:-http://127.0.0.1:19545}"
PUBLIC_LOCAL_RPC_URL="${PUBLIC_LOCAL_RPC_URL:-http://127.0.0.1:29645}"
REMOTE_HOST="${REMOTE_HOST:-139.180.207.66}"
REMOTE_USER="${REMOTE_USER:-root}"
REMOTE_RPC_PORT="${REMOTE_RPC_PORT:-28545}"
SSH_KEY="${SSH_KEY:-/root/.ssh/agent_139_180_207_66}"

RPC_TIMEOUT_SECS="${RPC_TIMEOUT_SECS:-8}"
SSH_TIMEOUT_SECS="${SSH_TIMEOUT_SECS:-8}"
EXPECTED_NET_VERSION="${EXPECTED_NET_VERSION:-10086}"
MIN_PUBLIC_PEERS="${MIN_PUBLIC_PEERS:-1}"
MAX_PUBLIC_BLOCK_GAP="${MAX_PUBLIC_BLOCK_GAP:-0}"
MIN_PUBLIC_BLOCK_HEIGHT="${MIN_PUBLIC_BLOCK_HEIGHT:-0}"
EXPECTED_REMOTE_ENDPOINT_ON_LOCAL="${EXPECTED_REMOTE_ENDPOINT_ON_LOCAL:-139.180.207.66:30303}"

MONITOR_SCRIPT="${ROOT_DIR}/scripts/public_node_soak_monitor.sh"

FAILURES=0

log_pass() {
  printf '[PASS] %s\n' "$1"
}

log_fail() {
  printf '[FAIL] %s\n' "$1"
  FAILURES=$((FAILURES + 1))
}

rpc_local() {
  local url="$1"
  local method="$2"
  curl -fsS --max-time "${RPC_TIMEOUT_SECS}" \
    -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":[],\"id\":1}" \
    "${url}"
}

rpc_remote() {
  local method="$1"
  ssh \
    -i "${SSH_KEY}" \
    -o StrictHostKeyChecking=no \
    -o BatchMode=yes \
    -o ConnectTimeout="${SSH_TIMEOUT_SECS}" \
    "${REMOTE_USER}@${REMOTE_HOST}" \
    "curl -fsS --max-time ${RPC_TIMEOUT_SECS} -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":[],\"id\":1}' http://127.0.0.1:${REMOTE_RPC_PORT}"
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
    return 0
  fi
  printf '%d\n' "$((16#${hex}))"
}

safe_extract_result_hex() {
  local json="$1"
  local v
  v="$(printf '%s' "${json}" | extract_result_hex)"
  if [[ -z "${v}" ]]; then
    printf 'N/A\n'
  else
    printf '%s\n' "${v}"
  fi
}

safe_extract_block_hex() {
  local json="$1"
  local v
  v="$(printf '%s' "${json}" | extract_block_hex)"
  if [[ -z "${v}" ]]; then
    printf 'N/A\n'
  else
    printf '%s\n' "${v}"
  fi
}

printf 'Node Sync Check @ %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf 'mining_rpc=%s\n' "${MINING_RPC_URL}"
printf 'public_local_rpc=%s\n' "${PUBLIC_LOCAL_RPC_URL}"
printf 'public_remote_rpc=%s@%s:%s\n' "${REMOTE_USER}" "${REMOTE_HOST}" "${REMOTE_RPC_PORT}"
printf '\n'

mining_net_json=''
mining_peer_json=''
mining_block_json=''
if mining_net_json="$(rpc_local "${MINING_RPC_URL}" net_version 2>/dev/null)" && \
   mining_peer_json="$(rpc_local "${MINING_RPC_URL}" net_peerCount 2>/dev/null)" && \
   mining_block_json="$(rpc_local "${MINING_RPC_URL}" rabbit_getLatestBlock 2>/dev/null)"; then
  log_pass "主挖矿节点 RPC 可达"
else
  log_fail "主挖矿节点 RPC 不可达 (${MINING_RPC_URL})"
fi

local_net_json=''
local_peer_json=''
local_block_json=''
local_rabbit_peers_json=''
if local_net_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" net_version 2>/dev/null)" && \
   local_peer_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" net_peerCount 2>/dev/null)" && \
   local_block_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" rabbit_getLatestBlock 2>/dev/null)" && \
   local_rabbit_peers_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" rabbit_peers 2>/dev/null)"; then
  log_pass "本地公网节点 RPC 可达"
else
  log_fail "本地公网节点 RPC 不可达 (${PUBLIC_LOCAL_RPC_URL})"
fi

remote_net_json=''
remote_peer_json=''
remote_block_json=''
remote_rabbit_peers_json=''
if remote_net_json="$(rpc_remote net_version 2>/dev/null)" && \
   remote_peer_json="$(rpc_remote net_peerCount 2>/dev/null)" && \
   remote_block_json="$(rpc_remote rabbit_getLatestBlock 2>/dev/null)" && \
   remote_rabbit_peers_json="$(rpc_remote rabbit_peers 2>/dev/null)"; then
  log_pass "远端公网节点 RPC 可达"
else
  log_fail "远端公网节点 RPC 不可达 (${REMOTE_HOST}:${REMOTE_RPC_PORT})"
fi

mining_net="$(safe_extract_result_hex "${mining_net_json}")"
mining_peers_hex="$(safe_extract_result_hex "${mining_peer_json}")"
mining_block_hex="$(safe_extract_block_hex "${mining_block_json}")"

local_net="$(safe_extract_result_hex "${local_net_json}")"
local_peers_hex="$(safe_extract_result_hex "${local_peer_json}")"
local_block_hex="$(safe_extract_block_hex "${local_block_json}")"

remote_net="$(safe_extract_result_hex "${remote_net_json}")"
remote_peers_hex="$(safe_extract_result_hex "${remote_peer_json}")"
remote_block_hex="$(safe_extract_block_hex "${remote_block_json}")"

printf '\nSnapshot:\n'
printf '  mining-main   net=%s peers=%s block=%s\n' "${mining_net}" "${mining_peers_hex}" "${mining_block_hex}"
printf '  public-local  net=%s peers=%s block=%s\n' "${local_net}" "${local_peers_hex}" "${local_block_hex}"
printf '  public-remote net=%s peers=%s block=%s\n' "${remote_net}" "${remote_peers_hex}" "${remote_block_hex}"

if [[ "${local_net}" == "${EXPECTED_NET_VERSION}" && "${remote_net}" == "${EXPECTED_NET_VERSION}" ]]; then
  log_pass "公网节点 net_version 一致且为 ${EXPECTED_NET_VERSION}"
else
  log_fail "公网节点 net_version 异常 (local=${local_net}, remote=${remote_net}, expect=${EXPECTED_NET_VERSION})"
fi

if [[ "${local_peers_hex}" != "N/A" && "${remote_peers_hex}" != "N/A" ]]; then
  local_peers_dec="$(hex_to_dec "${local_peers_hex}")"
  remote_peers_dec="$(hex_to_dec "${remote_peers_hex}")"
  if (( local_peers_dec >= MIN_PUBLIC_PEERS && remote_peers_dec >= MIN_PUBLIC_PEERS )); then
    log_pass "公网节点 peerCount 达标 (local=${local_peers_dec}, remote=${remote_peers_dec})"
  else
    log_fail "公网节点 peerCount 不达标 (local=${local_peers_dec}, remote=${remote_peers_dec}, min=${MIN_PUBLIC_PEERS})"
  fi
else
  log_fail "无法解析公网节点 peerCount"
fi

if [[ "${local_block_hex}" != "N/A" && "${remote_block_hex}" != "N/A" ]]; then
  local_block_dec="$(hex_to_dec "${local_block_hex}")"
  remote_block_dec="$(hex_to_dec "${remote_block_hex}")"
  gap=$(( local_block_dec - remote_block_dec ))
  if (( gap < 0 )); then
    gap=$(( -gap ))
  fi

  if (( gap <= MAX_PUBLIC_BLOCK_GAP )); then
    log_pass "公网节点区块高度差达标 (gap=${gap})"
  else
    log_fail "公网节点区块高度差超阈值 (local=${local_block_dec}, remote=${remote_block_dec}, gap=${gap}, max=${MAX_PUBLIC_BLOCK_GAP})"
  fi

  if (( local_block_dec >= MIN_PUBLIC_BLOCK_HEIGHT && remote_block_dec >= MIN_PUBLIC_BLOCK_HEIGHT )); then
    log_pass "公网节点区块高度下限达标 (local=${local_block_dec}, remote=${remote_block_dec}, min=${MIN_PUBLIC_BLOCK_HEIGHT})"
  else
    log_fail "公网节点区块高度下限不达标 (local=${local_block_dec}, remote=${remote_block_dec}, min=${MIN_PUBLIC_BLOCK_HEIGHT})"
  fi
else
  log_fail "无法解析公网节点区块高度"
fi

if [[ -n "${local_rabbit_peers_json}" ]] && printf '%s' "${local_rabbit_peers_json}" | grep -q "${EXPECTED_REMOTE_ENDPOINT_ON_LOCAL}"; then
  log_pass "本地公网节点 rabbit_peers 包含远端端点 ${EXPECTED_REMOTE_ENDPOINT_ON_LOCAL}"
else
  log_fail "本地公网节点 rabbit_peers 未包含远端端点 ${EXPECTED_REMOTE_ENDPOINT_ON_LOCAL}"
fi

monitor_status=''
if [[ -x "${MONITOR_SCRIPT}" ]] && monitor_status="$("${MONITOR_SCRIPT}" status 2>/dev/null)"; then
  log_pass "公网 soak 监控脚本可用"
else
  log_fail "公网 soak 监控脚本不可用"
  monitor_status=''
fi

if [[ -n "${monitor_status}" ]]; then
  local_ok="$(printf '%s\n' "${monitor_status}" | sed -n 's/^LOCAL_OK=//p' | tail -n1)"
  remote_ok="$(printf '%s\n' "${monitor_status}" | sed -n 's/^REMOTE_OK=//p' | tail -n1)"
  local_node_alive="$(printf '%s\n' "${monitor_status}" | sed -n 's/^LOCAL_NODE_ALIVE=//p' | tail -n1)"
  remote_node_alive="$(printf '%s\n' "${monitor_status}" | sed -n 's/^REMOTE_NODE_ALIVE=//p' | tail -n1)"
  local_drop="$(printf '%s\n' "${monitor_status}" | sed -n 's/^LOCAL_DROP_EVENTS=//p' | tail -n1)"
  remote_drop="$(printf '%s\n' "${monitor_status}" | sed -n 's/^REMOTE_DROP_EVENTS=//p' | tail -n1)"
  local_rpc_err="$(printf '%s\n' "${monitor_status}" | sed -n 's/^LOCAL_RPC_ERRORS=//p' | tail -n1)"
  remote_rpc_err="$(printf '%s\n' "${monitor_status}" | sed -n 's/^REMOTE_RPC_ERRORS=//p' | tail -n1)"
  ssh_err="$(printf '%s\n' "${monitor_status}" | sed -n 's/^SSH_ERRORS=//p' | tail -n1)"

  if [[ "${local_ok}" == "1" && "${remote_ok}" == "1" && "${local_node_alive}" == "1" && "${remote_node_alive}" == "1" ]]; then
    log_pass "soak 监控健康位正常 (local_ok=${local_ok}, remote_ok=${remote_ok})"
  else
    log_fail "soak 监控健康位异常 (local_ok=${local_ok}, remote_ok=${remote_ok}, local_alive=${local_node_alive}, remote_alive=${remote_node_alive})"
  fi

  if [[ "${local_drop}" == "0" && "${remote_drop}" == "0" && "${local_rpc_err}" == "0" && "${remote_rpc_err}" == "0" && "${ssh_err}" == "0" ]]; then
    log_pass "soak 监控错误计数为 0"
  else
    log_fail "soak 监控存在错误计数 (local_drop=${local_drop}, remote_drop=${remote_drop}, local_rpc_err=${local_rpc_err}, remote_rpc_err=${remote_rpc_err}, ssh_err=${ssh_err})"
  fi
fi

printf '\nSummary: failures=%d\n' "${FAILURES}"
if (( FAILURES > 0 )); then
  exit 1
fi
