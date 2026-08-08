#!/usr/bin/env bash
# RabbitChain 本地双节点挖矿测试脚本
# 测试：节点同步、区块传播、分叉处理逻辑
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${ROOT_DIR}/target/release/rabbitchain"
TEST_DIR="${ROOT_DIR}/target/test-data"
LOG_DIR="${TEST_DIR}/logs"
AUTH_TOKEN="rabbit-two-node-test-token"
COINBASE="0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9"

mkdir -p "${LOG_DIR}"

cleanup() {
    echo ""
    echo "=== Cleaning up ==="
    for pid_file in "${LOG_DIR}"/*.pid; do
        if [ -f "${pid_file}" ]; then
            pid=$(cat "${pid_file}")
            kill "${pid}" 2>/dev/null || true
        fi
    done
    pkill -f "rabbitchain" 2>/dev/null || true
    wait 2>/dev/null || true
    echo "=== Cleanup complete ==="
}
trap cleanup EXIT INT TERM

# RPC call with auth
rpc_call() {
    local url="$1" method="$2" params="${3:-[]}" token="${4:-$AUTH_TOKEN}"
    local payload
    payload="$(printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "${method}" "${params}")"
    local args=(-fsS --max-time 5 -H 'Content-Type: application/json' --data "${payload}")
    [ -n "${token}" ] && args+=(-H "authorization: Bearer ${token}" -H "x-rabbit-token: ${token}")
    curl "${args[@]}" "${url}" 2>/dev/null || echo '{"error":"curl_failed"}'
}

# Get latest block info
get_latest_info() {
    local url="$1" token="${2:-$AUTH_TOKEN}"
    local json
    json="$(rpc_call "${url}" "rabbit_getLatestBlock" '[]' "${token}")"
    local height_hex hash block_hash
    height_hex="$(echo "${json}" | jq -r '.result.body.number // "0x0"' 2>/dev/null)"
    hash="$(echo "${json}" | jq -r '.result.hash // "0x0"' 2>/dev/null)"
    block_hash="$(echo "${json}" | jq -r '.result.body.block_hash // "0x0"' 2>/dev/null)"
    local height_dec=$((16#${height_hex#0x}))
    echo "${height_dec}|${hash}|${block_hash}"
}

# Wait for RPC with auth
wait_rpc() {
    local port="$1" timeout="${2:-60}" token="${3:-$AUTH_TOKEN}" i=0
    while [ "${i}" -lt "${timeout}" ]; do
        if rpc_call "http://127.0.0.1:${port}" "net_version" '[]' "${token}" 2>/dev/null | grep -q "result"; then
            return 0
        fi
        sleep 1
        i=$((i + 1))
    done
    echo "Timeout RPC port ${port}" >&2
    return 1
}

echo "=============================================="
echo "  RabbitChain 本地双节点挖矿测试"
echo "=============================================="
echo ""

if [ ! -x "${BIN}" ]; then
    echo "ERROR: Binary not found at ${BIN}"
    exit 1
fi

# Clean start
cleanup
sleep 1
rm -rf "${TEST_DIR}/node-a" "${TEST_DIR}/node-b" "${LOG_DIR}"
mkdir -p "${TEST_DIR}/node-a" "${TEST_DIR}/node-b" "${LOG_DIR}"

echo "=== Phase 1: Starting Node A (Boot Mining Node, port 18545) ==="
"${BIN}" --data-dir "${TEST_DIR}/node-a" run \
    --http-port 18545 --ws-port 18546 \
    --mine --coinbase "${COINBASE}" \
    --rpc-auth-token "${AUTH_TOKEN}" --rpc-rate-limit-per-minute 0 \
    --p2p-listen-addr 127.0.0.1 --p2p-listen-port 30303 \
    --disable-p2p-ws --max-peers 10 \
    --p2p-peer-id "node-a" \
    --p2p-sync-blocks-path "${TEST_DIR}/node-a/p2p-blocks.jsonl" \
    > "${LOG_DIR}/node-a.log" 2>&1 &
echo $! > "${LOG_DIR}/node-a.pid"
wait_rpc 18545 60
echo "Node A ready"

# Let Node A mine to at least height 5
echo "Waiting for Node A to mine to height 5+..."
for i in $(seq 1 60); do
    info=$(get_latest_info "http://127.0.0.1:18545")
    height=$(echo "${info}" | cut -d'|' -f1)
    echo "[${i}s] Node A height=${height}"
    if [ "${height}" -ge 5 ] 2>/dev/null; then
        echo "✅ Node A at height ${height}"
        break
    fi
    sleep 2
done

echo ""
echo "=== Phase 2: Starting Node B (Follower, port 28545) ==="
"${BIN}" --data-dir "${TEST_DIR}/node-b" run \
    --http-port 28545 --ws-port 28546 \
    --rpc-auth-token "${AUTH_TOKEN}" --rpc-rate-limit-per-minute 0 \
    --p2p-listen-addr 127.0.0.1 --p2p-listen-port 31303 \
    --bootnode "enode://node-a@127.0.0.1:30303" \
    --disable-p2p-ws --max-peers 10 \
    --p2p-peer-id "node-b" \
    --p2p-sync-blocks-path "${TEST_DIR}/node-b/p2p-blocks.jsonl" \
    > "${LOG_DIR}/node-b.log" 2>&1 &
echo $! > "${LOG_DIR}/node-b.pid"
wait_rpc 28545 60
echo "Node B ready"

echo ""
echo "=== Phase 3: Monitoring Sync ==="
for i in $(seq 1 30); do
    info_a=$(get_latest_info "http://127.0.0.1:18545")
    info_b=$(get_latest_info "http://127.0.0.1:28545")
    ha=$(echo "${info_a}" | cut -d'|' -f1)
    hb=$(echo "${info_b}" | cut -d'|' -f1)
    hash_a=$(echo "${info_a}" | cut -d'|' -f2)
    hash_b=$(echo "${info_b}" | cut -d'|' -f2)
    gap=$((ha - hb))
    echo "$(date +%H:%M:%S) A=${ha} B=${hb} gap=${gap}"
    
    if [ "${hb}" -gt 0 ] 2>/dev/null && [ "${gap}" -le 1 ] 2>/dev/null; then
        echo ""
        echo "  >> Both nodes synced within ${gap} blocks!"
        if [ "${hash_a}" = "${hash_b}" ] 2>/dev/null; then
            echo "  >> ✅ SAME HASH - no fork"
        else
            echo "  >> Checking block-by-block..."
        fi
        break
    fi
    sleep 3
done

echo ""
echo "=== Phase 4: Block-by-block Verification ==="
info_a=$(get_latest_info "http://127.0.0.1:18545")
info_b=$(get_latest_info "http://127.0.0.1:28545")
ha=$(echo "${info_a}" | cut -d'|' -f1)
hb=$(echo "${info_b}" | cut -d'|' -f1)
echo "Node A: height=${ha}   Node B: height=${hb}"
min=$(( ha < hb ? ha : hb ))

forks=0
for h in $(seq 0 "${min}"); do
    json_a=$(rpc_call "http://127.0.0.1:18545" "rabbit_getBlockByNumber" "[${h}]")
    json_b=$(rpc_call "http://127.0.0.1:28545" "rabbit_getBlockByNumber" "[${h}]")
    ha_a=$(echo "${json_a}" | jq -r '.result.hash // "MISSING"' 2>/dev/null)
    ha_b=$(echo "${json_b}" | jq -r '.result.hash // "MISSING"' 2>/dev/null)
    if [ "${ha_a}" != "${ha_b}" ]; then
        echo "❌ FORK at ${h}: A=${ha_a:0:24}... B=${ha_b:0:24}..."
        forks=$((forks + 1))
    fi
done

if [ "${forks}" -eq 0 ]; then
    echo "✅ All ${min} blocks consistent"
else
    echo "❌ ${forks} fork(s) detected"
fi

echo ""
echo "=== Phase 5: Network Connectivity ==="
peers_a=$(rpc_call "http://127.0.0.1:18545" "rabbit_peers" '[]' 2>/dev/null | jq '.result | length' 2>/dev/null || echo "?")
peers_b=$(rpc_call "http://127.0.0.1:28545" "rabbit_peers" '[]' 2>/dev/null | jq '.result | length' 2>/dev/null || echo "?")
echo "Node A peers: ${peers_a}   Node B peers: ${peers_b}"

echo ""
echo "=============================================="
echo "  TEST COMPLETE"
echo "=============================================="
echo "Node A height: $(echo "$(get_latest_info "http://127.0.0.1:18545")" | cut -d'|' -f1)"
echo "Node B height: $(echo "$(get_latest_info "http://127.0.0.1:28545")" | cut -d'|' -f1)"
echo "Logs: ${LOG_DIR}"
