#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

PUBLIC_LOCAL_RPC_URL="${PUBLIC_LOCAL_RPC_URL:-http://127.0.0.1:29645}"
OBSERVER_RPC_URL="${OBSERVER_RPC_URL:-http://127.0.0.1:39745}"
REMOTE_HOST="${REMOTE_HOST:-139.180.207.66}"
REMOTE_USER="${REMOTE_USER:-root}"
REMOTE_RPC_PORT="${REMOTE_RPC_PORT:-28545}"
SSH_KEY="${SSH_KEY:-/root/.ssh/agent_139_180_207_66}"
EXPLORER_BASE_URL="${EXPLORER_BASE_URL:-http://${REMOTE_HOST}:19080}"

EXPECTED_NET_VERSION="${EXPECTED_NET_VERSION:-10086}"
MIN_PUBLIC_PEERS="${MIN_PUBLIC_PEERS:-1}"
MIN_OBSERVER_PEERS="${MIN_OBSERVER_PEERS:-1}"
MAX_PUBLIC_BLOCK_GAP="${MAX_PUBLIC_BLOCK_GAP:-4}"
MIN_PUBLIC_BLOCK_HEIGHT="${MIN_PUBLIC_BLOCK_HEIGHT:-1}"
MIN_OBSERVER_BLOCK_HEIGHT="${MIN_OBSERVER_BLOCK_HEIGHT:-1}"
REQUIRE_OBSERVER_SYNC="${REQUIRE_OBSERVER_SYNC:-1}"
MAX_OBSERVER_SYNC_LAG="${MAX_OBSERVER_SYNC_LAG:-2}"
CHECK_ADDRESS="${CHECK_ADDRESS:-0x1111111111111111111111111111111111111111}"
CHECK_TX_HASH="${CHECK_TX_HASH:-}"

RPC_TIMEOUT_SECS="${RPC_TIMEOUT_SECS:-8}"
SSH_TIMEOUT_SECS="${SSH_TIMEOUT_SECS:-8}"
REMOTE_RPC_RETRIES="${REMOTE_RPC_RETRIES:-3}"
HTTP_TIMEOUT_SECS="${HTTP_TIMEOUT_SECS:-8}"
HTTP_RETRIES="${HTTP_RETRIES:-3}"

FAILURES=0

log_pass() {
  printf '[PASS] %s\n' "$1"
}

log_fail() {
  printf '[FAIL] %s\n' "$1"
  FAILURES=$((FAILURES + 1))
}

check_no_sync_source_flag() {
  local scope="$1"
  local cmdline="$2"
  if [[ -z "${cmdline}" ]]; then
    log_fail "${scope} 进程命令行为空，无法校验是否含 sync-source-rpc"
    return
  fi
  if [[ "${cmdline}" == *"--sync-source-rpc"* ]]; then
    log_fail "${scope} 启动参数包含 --sync-source-rpc（旁路同步）"
  else
    log_pass "${scope} 未使用 --sync-source-rpc（纯 P2P 同步）"
  fi
}

rpc_local() {
  local url="$1"
  local method="$2"
  local params="${3:-[]}"
  curl -fsS --max-time "${RPC_TIMEOUT_SECS}" \
    -H 'Content-Type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":${params},\"id\":1}" \
    "${url}"
}

rpc_remote() {
  local method="$1"
  local params="${2:-[]}"
  local attempt=1
  while (( attempt <= REMOTE_RPC_RETRIES )); do
    if ssh \
      -i "${SSH_KEY}" \
      -o StrictHostKeyChecking=no \
      -o BatchMode=yes \
      -o ConnectTimeout="${SSH_TIMEOUT_SECS}" \
      "${REMOTE_USER}@${REMOTE_HOST}" \
      "curl -fsS --max-time ${RPC_TIMEOUT_SECS} -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":${params},\"id\":1}' http://127.0.0.1:${REMOTE_RPC_PORT}"; then
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 1
  done
  return 1
}

ssh_remote_with_retries() {
  local remote_cmd="$1"
  local attempt=1
  while (( attempt <= REMOTE_RPC_RETRIES )); do
    if ssh \
      -i "${SSH_KEY}" \
      -o StrictHostKeyChecking=no \
      -o BatchMode=yes \
      -o ConnectTimeout="${SSH_TIMEOUT_SECS}" \
      "${REMOTE_USER}@${REMOTE_HOST}" \
      "${remote_cmd}"; then
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 1
  done
  return 1
}

http_check() {
  local url="$1"
  local attempt=1
  while (( attempt <= HTTP_RETRIES )); do
    if curl -fsS --max-time "${HTTP_TIMEOUT_SECS}" "${url}" >/dev/null 2>&1; then
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 1
  done
  return 1
}

http_get_with_retries() {
  local url="$1"
  local attempt=1
  local body=""
  while (( attempt <= HTTP_RETRIES )); do
    if body="$(curl -fsS --max-time "${HTTP_TIMEOUT_SECS}" "${url}" 2>/dev/null)"; then
      printf '%s' "${body}"
      return 0
    fi
    attempt=$((attempt + 1))
    sleep 1
  done
  return 1
}

extract_result_hex() {
  sed -n 's/.*"result":"\([^"]*\)".*/\1/p'
}

extract_block_hex() {
  sed -n 's/.*"number":"\([^"]*\)".*/\1/p'
}

extract_sync_field_u64() {
  local field="$1"
  sed -n "s/.*\"${field}\":[ ]*\\([0-9][0-9]*\\).*/\\1/p"
}

extract_sync_field_bool() {
  sed -n 's/.*"syncing":[ ]*\(true\|false\).*/\1/p'
}

extract_account_balance() {
  sed -n 's/.*"balance":"\([^"]*\)".*/\1/p'
}

extract_account_nonce() {
  sed -n 's/.*"nonce":"\([^"]*\)".*/\1/p'
}

extract_tx_block_number() {
  sed -n 's/.*"block_number":[ ]*\([0-9][0-9]*\).*/\1/p'
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

printf 'Mainnet Checklist @ %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf 'public_local_rpc=%s\n' "${PUBLIC_LOCAL_RPC_URL}"
printf 'observer_rpc=%s\n' "${OBSERVER_RPC_URL}"
printf 'public_remote_rpc=%s@%s:%s\n' "${REMOTE_USER}" "${REMOTE_HOST}" "${REMOTE_RPC_PORT}"
printf 'explorer=%s\n' "${EXPLORER_BASE_URL}"
printf '\n'

local_net_json=''; local_peer_json=''; local_block_json=''; local_sync_json=''; local_rabbit_peers_json=''
if local_net_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" net_version 2>/dev/null)" && \
   local_peer_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" net_peerCount 2>/dev/null)" && \
   local_block_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" rabbit_getLatestBlock 2>/dev/null)" && \
   local_sync_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" rabbit_syncStatus 2>/dev/null)" && \
   local_rabbit_peers_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" rabbit_peers 2>/dev/null)"; then
  log_pass "本地公网节点 RPC 可达"
else
  log_fail "本地公网节点 RPC 不可达 (${PUBLIC_LOCAL_RPC_URL})"
fi

remote_net_json=''; remote_peer_json=''; remote_block_json=''; remote_sync_json=''
if remote_net_json="$(rpc_remote net_version 2>/dev/null)" && \
   remote_peer_json="$(rpc_remote net_peerCount 2>/dev/null)" && \
   remote_block_json="$(rpc_remote rabbit_getLatestBlock 2>/dev/null)" && \
   remote_sync_json="$(rpc_remote rabbit_syncStatus 2>/dev/null)"; then
  log_pass "远端公网节点 RPC 可达"
else
  log_fail "远端公网节点 RPC 不可达 (${REMOTE_HOST}:${REMOTE_RPC_PORT})"
fi

observer_net_json=''; observer_peer_json=''; observer_block_json=''; observer_sync_json=''
if observer_net_json="$(rpc_local "${OBSERVER_RPC_URL}" net_version 2>/dev/null)" && \
   observer_peer_json="$(rpc_local "${OBSERVER_RPC_URL}" net_peerCount 2>/dev/null)" && \
   observer_block_json="$(rpc_local "${OBSERVER_RPC_URL}" rabbit_getLatestBlock 2>/dev/null)" && \
   observer_sync_json="$(rpc_local "${OBSERVER_RPC_URL}" rabbit_syncStatus 2>/dev/null)"; then
  log_pass "observer 节点 RPC 可达"
else
  log_fail "observer 节点 RPC 不可达 (${OBSERVER_RPC_URL})"
fi

if [[ -f "${ROOT_DIR}/artifacts/public-node-soak/current.env" ]]; then
  # shellcheck disable=SC1091
  source "${ROOT_DIR}/artifacts/public-node-soak/current.env" || true
  if [[ -n "${NODE_PID:-}" ]]; then
    local_cmdline="$(ps -p "${NODE_PID}" -o args= 2>/dev/null || true)"
    check_no_sync_source_flag "public-local" "${local_cmdline}"
  else
    log_fail "public-local current.env 未包含 NODE_PID"
  fi
else
  log_fail "public-local current.env 不存在"
fi

if [[ -f "${ROOT_DIR}/artifacts/public-node-observer/current.env" ]]; then
  # shellcheck disable=SC1091
  source "${ROOT_DIR}/artifacts/public-node-observer/current.env" || true
  if [[ -n "${NODE_PID:-}" ]]; then
    observer_cmdline="$(ps -p "${NODE_PID}" -o args= 2>/dev/null || true)"
    check_no_sync_source_flag "observer" "${observer_cmdline}"
  else
    log_fail "observer current.env 未包含 NODE_PID"
  fi
else
  log_fail "observer current.env 不存在"
fi

remote_cmdline="$(
  ssh_remote_with_retries \
    'set -euo pipefail
if [ -f /root/works/Rabbit-Chain-node-public-soak/current.env ]; then
  . /root/works/Rabbit-Chain-node-public-soak/current.env || true
  if [ -n "${NODE_PID:-}" ]; then
    ps -p "${NODE_PID}" -o args= 2>/dev/null || true
  fi
fi' \
    2>/dev/null || true
)"
check_no_sync_source_flag "public-remote" "${remote_cmdline}"

local_net="$(safe_extract_result_hex "${local_net_json}")"
local_peers_hex="$(safe_extract_result_hex "${local_peer_json}")"
local_block_hex="$(safe_extract_block_hex "${local_block_json}")"

remote_net="$(safe_extract_result_hex "${remote_net_json}")"
remote_peers_hex="$(safe_extract_result_hex "${remote_peer_json}")"
remote_block_hex="$(safe_extract_block_hex "${remote_block_json}")"

observer_net="$(safe_extract_result_hex "${observer_net_json}")"
observer_peers_hex="$(safe_extract_result_hex "${observer_peer_json}")"
observer_block_hex="$(safe_extract_block_hex "${observer_block_json}")"

printf '\nSnapshot:\n'
printf '  public-local   net=%s peers=%s block=%s\n' "${local_net}" "${local_peers_hex}" "${local_block_hex}"
printf '  public-remote  net=%s peers=%s block=%s\n' "${remote_net}" "${remote_peers_hex}" "${remote_block_hex}"
printf '  observer       net=%s peers=%s block=%s\n' "${observer_net}" "${observer_peers_hex}" "${observer_block_hex}"

if [[ "${local_net}" == "${EXPECTED_NET_VERSION}" && "${remote_net}" == "${EXPECTED_NET_VERSION}" && "${observer_net}" == "${EXPECTED_NET_VERSION}" ]]; then
  log_pass "三节点 net_version 一致且为 ${EXPECTED_NET_VERSION}"
else
  log_fail "net_version 异常 (local=${local_net}, remote=${remote_net}, observer=${observer_net}, expect=${EXPECTED_NET_VERSION})"
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

if [[ "${observer_peers_hex}" != "N/A" ]]; then
  observer_peers_dec="$(hex_to_dec "${observer_peers_hex}")"
  if (( observer_peers_dec >= MIN_OBSERVER_PEERS )); then
    log_pass "observer peerCount 达标 (${observer_peers_dec})"
  else
    log_fail "observer peerCount 不达标 (${observer_peers_dec}, min=${MIN_OBSERVER_PEERS})"
  fi
else
  log_fail "无法解析 observer peerCount"
fi

if [[ "${local_block_hex}" != "N/A" && "${remote_block_hex}" != "N/A" ]]; then
  local_block_dec="$(hex_to_dec "${local_block_hex}")"
  remote_block_dec="$(hex_to_dec "${remote_block_hex}")"
  gap=$(( local_block_dec - remote_block_dec ))
  if (( gap < 0 )); then gap=$(( -gap )); fi

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

if [[ "${observer_block_hex}" != "N/A" ]]; then
  observer_block_dec="$(hex_to_dec "${observer_block_hex}")"
  if (( observer_block_dec >= MIN_OBSERVER_BLOCK_HEIGHT )); then
    log_pass "observer 区块高度下限达标 (${observer_block_dec})"
  else
    log_fail "observer 区块高度下限不达标 (${observer_block_dec}, min=${MIN_OBSERVER_BLOCK_HEIGHT})"
  fi
else
  log_fail "无法解析 observer 区块高度"
fi

if [[ -n "${local_sync_json}" ]]; then
  local_syncing="$(printf '%s' "${local_sync_json}" | extract_sync_field_bool)"
  local_sync_local="$(printf '%s' "${local_sync_json}" | extract_sync_field_u64 local_head)"
  local_sync_net="$(printf '%s' "${local_sync_json}" | extract_sync_field_u64 network_head)"
  printf '  sync local: local_head=%s network_head=%s syncing=%s\n' "${local_sync_local:-N/A}" "${local_sync_net:-N/A}" "${local_syncing:-N/A}"
fi
if [[ -n "${remote_sync_json}" ]]; then
  remote_syncing="$(printf '%s' "${remote_sync_json}" | extract_sync_field_bool)"
  remote_sync_local="$(printf '%s' "${remote_sync_json}" | extract_sync_field_u64 local_head)"
  remote_sync_net="$(printf '%s' "${remote_sync_json}" | extract_sync_field_u64 network_head)"
  printf '  sync remote: local_head=%s network_head=%s syncing=%s\n' "${remote_sync_local:-N/A}" "${remote_sync_net:-N/A}" "${remote_syncing:-N/A}"
fi
if [[ -n "${observer_sync_json}" ]]; then
  observer_syncing="$(printf '%s' "${observer_sync_json}" | extract_sync_field_bool)"
  observer_sync_local="$(printf '%s' "${observer_sync_json}" | extract_sync_field_u64 local_head)"
  observer_sync_net="$(printf '%s' "${observer_sync_json}" | extract_sync_field_u64 network_head)"
  printf '  sync observer: local_head=%s network_head=%s syncing=%s\n' "${observer_sync_local:-N/A}" "${observer_sync_net:-N/A}" "${observer_syncing:-N/A}"
  if [[ "${REQUIRE_OBSERVER_SYNC}" == "1" && -n "${observer_sync_local}" && -n "${observer_sync_net}" ]]; then
    observer_lag=$(( observer_sync_net - observer_sync_local ))
    if (( observer_lag < 0 )); then
      observer_lag=0
    fi
    if (( observer_lag > MAX_OBSERVER_SYNC_LAG )); then
      log_fail "observer 同步滞后过大 (lag=${observer_lag}, max=${MAX_OBSERVER_SYNC_LAG})"
    else
      log_pass "observer 同步滞后可接受 (lag=${observer_lag})"
    fi
  fi
fi

local_account_json=''; remote_account_json=''
if local_account_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" rabbit_getAccount "[\"${CHECK_ADDRESS}\"]" 2>/dev/null)" && \
   remote_account_json="$(rpc_remote rabbit_getAccount "[\"${CHECK_ADDRESS}\"]" 2>/dev/null)"; then
  local_balance="$(printf '%s' "${local_account_json}" | extract_account_balance)"
  local_nonce="$(printf '%s' "${local_account_json}" | extract_account_nonce)"
  remote_balance="$(printf '%s' "${remote_account_json}" | extract_account_balance)"
  remote_nonce="$(printf '%s' "${remote_account_json}" | extract_account_nonce)"
  printf '  account %s: local(balance=%s nonce=%s) remote(balance=%s nonce=%s)\n' \
    "${CHECK_ADDRESS}" "${local_balance:-N/A}" "${local_nonce:-N/A}" "${remote_balance:-N/A}" "${remote_nonce:-N/A}"
  if [[ -n "${local_balance}" && -n "${remote_balance}" && "${local_balance}" == "${remote_balance}" && "${local_nonce}" == "${remote_nonce}" ]]; then
    log_pass "账户状态一致 (${CHECK_ADDRESS})"
  else
    log_fail "账户状态不一致 (${CHECK_ADDRESS})"
  fi
else
  log_fail "账户状态对比失败 (${CHECK_ADDRESS})"
fi

if [[ -n "${CHECK_TX_HASH}" ]]; then
  local_tx_json=''; remote_tx_json=''
  if local_tx_json="$(rpc_local "${PUBLIC_LOCAL_RPC_URL}" rabbit_getOperationByHash "[\"${CHECK_TX_HASH}\"]" 2>/dev/null)" && \
     remote_tx_json="$(rpc_remote rabbit_getOperationByHash "[\"${CHECK_TX_HASH}\"]" 2>/dev/null)"; then
    local_is_null=0
    remote_is_null=0
    [[ "${local_tx_json}" == *'"result":null'* ]] && local_is_null=1
    [[ "${remote_tx_json}" == *'"result":null'* ]] && remote_is_null=1
    local_block_num="$(printf '%s' "${local_tx_json}" | extract_tx_block_number)"
    remote_block_num="$(printf '%s' "${remote_tx_json}" | extract_tx_block_number)"
    printf '  tx %s: local_null=%d remote_null=%d local_block=%s remote_block=%s\n' \
      "${CHECK_TX_HASH}" "${local_is_null}" "${remote_is_null}" "${local_block_num:-N/A}" "${remote_block_num:-N/A}"
    if [[ "${local_is_null}" == "${remote_is_null}" ]]; then
      if [[ "${local_is_null}" == "0" && -n "${local_block_num}" && -n "${remote_block_num}" && "${local_block_num}" != "${remote_block_num}" ]]; then
        log_fail "交易索引块高不一致 (${CHECK_TX_HASH})"
      else
        log_pass "交易索引一致 (${CHECK_TX_HASH})"
      fi
    else
      log_fail "交易索引存在缺失 (${CHECK_TX_HASH})"
    fi
  else
    log_fail "交易索引对比失败 (${CHECK_TX_HASH})"
  fi
fi

if http_check "${EXPLORER_BASE_URL}/health"; then
  log_pass "区块浏览器 /health 可达"
else
  log_fail "区块浏览器 /health 不可达 (${EXPLORER_BASE_URL}/health)"
fi

if http_check "${EXPLORER_BASE_URL}/api/overview"; then
  log_pass "区块浏览器 /api/overview 可达"
else
  log_fail "区块浏览器 /api/overview 不可达"
fi

if recent_json="$(http_get_with_retries "${EXPLORER_BASE_URL}/api/txs/recent?limit=5")"; then
  recent_count="$(printf '%s' "${recent_json}" | sed -n 's/.*"items":\[\(.*\)\],"limit".*/\1/p' | awk -F'},{' '{print NF}' | awk 'NF==0{print 0} NF>0{print $1}')"
  log_pass "区块浏览器 /api/txs/recent 可达"
else
  log_fail "区块浏览器 /api/txs/recent 不可达"
fi

if http_check "${EXPLORER_BASE_URL}/api/accounts/${CHECK_ADDRESS}"; then
  log_pass "区块浏览器地址余额接口可达 (${CHECK_ADDRESS})"
else
  log_fail "区块浏览器地址余额接口不可达 (${CHECK_ADDRESS})"
fi

if http_check "${EXPLORER_BASE_URL}/api/accounts/${CHECK_ADDRESS}/txs?limit=5"; then
  log_pass "区块浏览器地址交易接口可达 (${CHECK_ADDRESS})"
else
  log_fail "区块浏览器地址交易接口不可达 (${CHECK_ADDRESS})"
fi

monitor_status=''
if [[ -x "${ROOT_DIR}/scripts/public_node_soak_monitor.sh" ]] && monitor_status="$("${ROOT_DIR}/scripts/public_node_soak_monitor.sh" status 2>/dev/null)"; then
  log_pass "公网 soak 监控脚本可用"
else
  log_fail "公网 soak 监控脚本不可用"
  monitor_status=''
fi

if [[ -n "${monitor_status}" ]]; then
  local_ok="$(printf '%s\n' "${monitor_status}" | sed -n 's/^LOCAL_OK=//p' | tail -n1)"
  remote_ok="$(printf '%s\n' "${monitor_status}" | sed -n 's/^REMOTE_OK=//p' | tail -n1)"
  local_rpc_err="$(printf '%s\n' "${monitor_status}" | sed -n 's/^LOCAL_RPC_ERRORS=//p' | tail -n1)"
  remote_rpc_err="$(printf '%s\n' "${monitor_status}" | sed -n 's/^REMOTE_RPC_ERRORS=//p' | tail -n1)"
  ssh_err="$(printf '%s\n' "${monitor_status}" | sed -n 's/^SSH_ERRORS=//p' | tail -n1)"

  if [[ "${local_ok}" == "1" && "${remote_ok}" == "1" ]]; then
    log_pass "soak 监控健康位正常"
  else
    log_fail "soak 监控健康位异常 (local_ok=${local_ok}, remote_ok=${remote_ok})"
  fi

  if [[ "${local_rpc_err}" == "0" && "${remote_rpc_err}" == "0" && "${ssh_err}" == "0" ]]; then
    log_pass "soak 监控 RPC/SSH 错误计数为 0"
  else
    log_fail "soak 监控存在 RPC/SSH 错误 (local_rpc_err=${local_rpc_err}, remote_rpc_err=${remote_rpc_err}, ssh_err=${ssh_err})"
  fi
fi

printf '\nSummary: failures=%d\n' "${FAILURES}"
if (( FAILURES > 0 )); then
  exit 1
fi
