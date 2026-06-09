#!/usr/bin/env bash
set -euo pipefail

BOOTNODE_RPC_URL="${BOOTNODE_RPC_URL:-http://127.0.0.1:8545}"
FOLLOWER_RPC_URL="${FOLLOWER_RPC_URL:-http://127.0.0.1:29645}"
OBSERVER_RPC_URL="${OBSERVER_RPC_URL:-http://127.0.0.1:39745}"
POOL_URL="${POOL_URL:-http://127.0.0.1:9332}"
MINER_METRICS_URL="${MINER_METRICS_URL:-http://127.0.0.1:9333}"
EXPLORER_BACKEND_URL="${EXPLORER_BACKEND_URL:-http://127.0.0.1:19080}"
RPC_AUTH_TOKEN="${RPC_AUTH_TOKEN:-}"

EXPECTED_NET_VERSION="${EXPECTED_NET_VERSION:-10086}"
MIN_FOLLOWER_PEERS="${MIN_FOLLOWER_PEERS:-1}"
MIN_OBSERVER_PEERS="${MIN_OBSERVER_PEERS:-1}"
MAX_FOLLOWER_BLOCK_GAP="${MAX_FOLLOWER_BLOCK_GAP:-2}"
MAX_OBSERVER_BLOCK_GAP="${MAX_OBSERVER_BLOCK_GAP:-2}"
MIN_POOL_SHARES="${MIN_POOL_SHARES:-1}"
MIN_MINER_ACCEPTED="${MIN_MINER_ACCEPTED:-1}"

RPC_TIMEOUT_SECS="${RPC_TIMEOUT_SECS:-8}"
HTTP_TIMEOUT_SECS="${HTTP_TIMEOUT_SECS:-8}"

FAILURES=0

log_pass() {
  printf '[PASS] %s\n' "$1"
}

log_fail() {
  printf '[FAIL] %s\n' "$1"
  FAILURES=$((FAILURES + 1))
}

rpc_call() {
  local url="$1"
  local method="$2"
  local curl_args=(
    -fsS --max-time "${RPC_TIMEOUT_SECS}"
    -H 'Content-Type: application/json'
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"${method}\",\"params\":[],\"id\":1}"
  )
  if [[ -n "${RPC_AUTH_TOKEN}" ]]; then
    curl_args+=(-H "authorization: Bearer ${RPC_AUTH_TOKEN}")
  fi
  curl "${curl_args[@]}" \
    "${url}"
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

printf 'Mainnet Local Check @ %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf 'bootnode_rpc=%s\n' "${BOOTNODE_RPC_URL}"
printf 'follower_rpc=%s\n' "${FOLLOWER_RPC_URL}"
printf 'observer_rpc=%s\n' "${OBSERVER_RPC_URL}"
printf 'pool=%s\n' "${POOL_URL}"
printf 'miner_metrics=%s\n' "${MINER_METRICS_URL}"
printf 'explorer=%s\n' "${EXPLORER_BACKEND_URL}"
printf '\n'

bootnode_net_json=''
bootnode_peer_json=''
bootnode_block_json=''
if bootnode_net_json="$(rpc_call "${BOOTNODE_RPC_URL}" net_version 2>/dev/null)" && \
   bootnode_peer_json="$(rpc_call "${BOOTNODE_RPC_URL}" net_peerCount 2>/dev/null)" && \
   bootnode_block_json="$(rpc_call "${BOOTNODE_RPC_URL}" rabbit_getLatestBlock 2>/dev/null)"; then
  log_pass "bootnode RPC 可达"
else
  log_fail "bootnode RPC 不可达 (${BOOTNODE_RPC_URL})"
fi

follower_net_json=''
follower_peer_json=''
follower_block_json=''
if follower_net_json="$(rpc_call "${FOLLOWER_RPC_URL}" net_version 2>/dev/null)" && \
   follower_peer_json="$(rpc_call "${FOLLOWER_RPC_URL}" net_peerCount 2>/dev/null)" && \
   follower_block_json="$(rpc_call "${FOLLOWER_RPC_URL}" rabbit_getLatestBlock 2>/dev/null)"; then
  log_pass "follower RPC 可达"
else
  log_fail "follower RPC 不可达 (${FOLLOWER_RPC_URL})"
fi

observer_net_json=''
observer_peer_json=''
observer_block_json=''
if observer_net_json="$(rpc_call "${OBSERVER_RPC_URL}" net_version 2>/dev/null)" && \
   observer_peer_json="$(rpc_call "${OBSERVER_RPC_URL}" net_peerCount 2>/dev/null)" && \
   observer_block_json="$(rpc_call "${OBSERVER_RPC_URL}" rabbit_getLatestBlock 2>/dev/null)"; then
  log_pass "observer RPC 可达"
else
  log_fail "observer RPC 不可达 (${OBSERVER_RPC_URL})"
fi

bootnode_net="$(printf '%s' "${bootnode_net_json}" | extract_result_hex || true)"
follower_net="$(printf '%s' "${follower_net_json}" | extract_result_hex || true)"
observer_net="$(printf '%s' "${observer_net_json}" | extract_result_hex || true)"

if [[ "${bootnode_net}" == "${EXPECTED_NET_VERSION}" && "${follower_net}" == "${EXPECTED_NET_VERSION}" && "${observer_net}" == "${EXPECTED_NET_VERSION}" ]]; then
  log_pass "三节点 net_version 一致且为 ${EXPECTED_NET_VERSION}"
else
  log_fail "net_version 异常 (bootnode=${bootnode_net:-N/A}, follower=${follower_net:-N/A}, observer=${observer_net:-N/A}, expect=${EXPECTED_NET_VERSION})"
fi

bootnode_peer_hex="$(printf '%s' "${bootnode_peer_json}" | extract_result_hex || true)"
follower_peer_hex="$(printf '%s' "${follower_peer_json}" | extract_result_hex || true)"
observer_peer_hex="$(printf '%s' "${observer_peer_json}" | extract_result_hex || true)"

follower_peer_dec="$(hex_to_dec "${follower_peer_hex:-0x0}")"
observer_peer_dec="$(hex_to_dec "${observer_peer_hex:-0x0}")"
if (( follower_peer_dec >= MIN_FOLLOWER_PEERS )); then
  log_pass "follower peerCount 达标 (${follower_peer_dec})"
else
  log_fail "follower peerCount 不达标 (${follower_peer_dec}, min=${MIN_FOLLOWER_PEERS})"
fi
if (( observer_peer_dec >= MIN_OBSERVER_PEERS )); then
  log_pass "observer peerCount 达标 (${observer_peer_dec})"
else
  log_fail "observer peerCount 不达标 (${observer_peer_dec}, min=${MIN_OBSERVER_PEERS})"
fi

bootnode_block_hex="$(printf '%s' "${bootnode_block_json}" | extract_block_hex || true)"
follower_block_hex="$(printf '%s' "${follower_block_json}" | extract_block_hex || true)"
observer_block_hex="$(printf '%s' "${observer_block_json}" | extract_block_hex || true)"

bootnode_block_dec="$(hex_to_dec "${bootnode_block_hex:-0x0}")"
follower_block_dec="$(hex_to_dec "${follower_block_hex:-0x0}")"
observer_block_dec="$(hex_to_dec "${observer_block_hex:-0x0}")"

follower_gap=$(( bootnode_block_dec - follower_block_dec ))
if (( follower_gap < 0 )); then follower_gap=$(( -follower_gap )); fi
observer_gap=$(( bootnode_block_dec - observer_block_dec ))
if (( observer_gap < 0 )); then observer_gap=$(( -observer_gap )); fi

if (( follower_gap <= MAX_FOLLOWER_BLOCK_GAP )); then
  log_pass "follower 区块高度差达标 (gap=${follower_gap})"
else
  log_fail "follower 区块高度差超阈值 (gap=${follower_gap}, max=${MAX_FOLLOWER_BLOCK_GAP})"
fi
if (( observer_gap <= MAX_OBSERVER_BLOCK_GAP )); then
  log_pass "observer 区块高度差达标 (gap=${observer_gap})"
else
  log_fail "observer 区块高度差超阈值 (gap=${observer_gap}, max=${MAX_OBSERVER_BLOCK_GAP})"
fi

pool_stats_json=''
if pool_stats_json="$(curl -fsS --max-time "${HTTP_TIMEOUT_SECS}" "${POOL_URL}/v1/stats" 2>/dev/null)"; then
  log_pass "pool stats 可达"
else
  log_fail "pool stats 不可达 (${POOL_URL}/v1/stats)"
fi

pool_shares="$(printf '%s' "${pool_stats_json}" | sed -n 's/.*"shares":{"[^"]*":[ ]*\([0-9][0-9]*\)}.*/\1/p')"
pool_shares="${pool_shares:-0}"
if (( pool_shares >= MIN_POOL_SHARES )); then
  log_pass "pool shares 达标 (${pool_shares})"
else
  log_fail "pool shares 不达标 (${pool_shares}, min=${MIN_POOL_SHARES})"
fi

miner_metrics=''
if miner_metrics="$(curl -fsS --max-time "${HTTP_TIMEOUT_SECS}" "${MINER_METRICS_URL}/metrics" 2>/dev/null)"; then
  log_pass "miner metrics 可达"
else
  log_fail "miner metrics 不可达 (${MINER_METRICS_URL}/metrics)"
fi

miner_accepted="$(printf '%s' "${miner_metrics}" | sed -n 's/.*rabbit_miner_shares_total{[^}]*status="accepted"} \([0-9][0-9]*\).*/\1/p' | tail -n1)"
miner_accepted="${miner_accepted:-0}"
if (( miner_accepted >= MIN_MINER_ACCEPTED )); then
  log_pass "miner accepted shares 达标 (${miner_accepted})"
else
  log_fail "miner accepted shares 不达标 (${miner_accepted}, min=${MIN_MINER_ACCEPTED})"
fi

if curl -fsS --max-time "${HTTP_TIMEOUT_SECS}" "${EXPLORER_BACKEND_URL}/health" >/dev/null 2>&1; then
  log_pass "explorer /health 可达"
else
  log_fail "explorer /health 不可达 (${EXPLORER_BACKEND_URL}/health)"
fi

if curl -fsS --max-time "${HTTP_TIMEOUT_SECS}" "${EXPLORER_BACKEND_URL}/api/overview" >/dev/null 2>&1; then
  log_pass "explorer /api/overview 可达"
else
  log_fail "explorer /api/overview 不可达 (${EXPLORER_BACKEND_URL}/api/overview)"
fi

printf '\nSummary: failures=%d\n' "${FAILURES}"
if (( FAILURES > 0 )); then
  exit 1
fi
