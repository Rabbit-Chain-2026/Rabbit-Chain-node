#!/usr/bin/env bash
set -euo pipefail

REMOTE_HOST="${REMOTE_HOST:-139.180.207.66}"
REMOTE_USER="${REMOTE_USER:-root}"
SSH_KEY="${SSH_KEY:-/root/.ssh/agent_139_180_207_66}"
SSH_TIMEOUT_SECS="${SSH_TIMEOUT_SECS:-10}"
REMOTE_RPC_PORT="${REMOTE_RPC_PORT:-28545}"
REMOTE_P2P_PORT="${REMOTE_P2P_PORT:-30303}"
REMOTE_RABBITCHAIN_BIN="${REMOTE_RABBITCHAIN_BIN:-/root/rabbitchain_local.mainnet_sync}"

FAILURES=0

log_pass() {
  printf '[PASS] %s\n' "$1"
}

log_fail() {
  printf '[FAIL] %s\n' "$1"
  FAILURES=$((FAILURES + 1))
}

ssh_cmd() {
  ssh -i "${SSH_KEY}" \
    -o StrictHostKeyChecking=no \
    -o BatchMode=yes \
    -o ConnectTimeout="${SSH_TIMEOUT_SECS}" \
    "${REMOTE_USER}@${REMOTE_HOST}" "$@"
}

printf 'Mainnet Remote Preflight @ %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
printf 'remote=%s@%s\n' "${REMOTE_USER}" "${REMOTE_HOST}"
printf 'remote_rpc_port=%s\n' "${REMOTE_RPC_PORT}"
printf 'remote_p2p_port=%s\n' "${REMOTE_P2P_PORT}"
printf 'remote_bin=%s\n\n' "${REMOTE_RABBITCHAIN_BIN}"

if ssh_cmd 'echo ok' >/dev/null 2>&1; then
  log_pass "SSH 可达"
else
  log_fail "SSH 不可达"
fi

if ssh_cmd "test -x '${REMOTE_RABBITCHAIN_BIN}'" >/dev/null 2>&1; then
  log_pass "远端 rabbitchain 二进制存在且可执行"
else
  log_fail "远端 rabbitchain 二进制不存在或不可执行 (${REMOTE_RABBITCHAIN_BIN})"
fi

if ssh_cmd "mkdir -p /root/works/Rabbit-Chain-node-public-soak && test -d /root/works/Rabbit-Chain-node-public-soak" >/dev/null 2>&1; then
  log_pass "远端工作目录可用"
else
  log_fail "远端工作目录不可用 (/root/works/Rabbit-Chain-node-public-soak)"
fi

if ssh_cmd "ss -ltn | grep -q ':${REMOTE_RPC_PORT}\\b'" >/dev/null 2>&1; then
  log_fail "远端 RPC 端口已占用 (${REMOTE_RPC_PORT})"
else
  log_pass "远端 RPC 端口空闲 (${REMOTE_RPC_PORT})"
fi

if ssh_cmd "ss -ltn | grep -q ':${REMOTE_P2P_PORT}\\b'" >/dev/null 2>&1; then
  log_fail "远端 P2P 端口已占用 (${REMOTE_P2P_PORT})"
else
  log_pass "远端 P2P 端口空闲 (${REMOTE_P2P_PORT})"
fi

printf '\nSummary: failures=%d\n' "${FAILURES}"
if (( FAILURES > 0 )); then
  exit 1
fi
