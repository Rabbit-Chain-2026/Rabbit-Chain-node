#!/usr/bin/env bash
# RabbitChain 分叉场景测试脚本
# 1. 启动 Node A (挖矿 bootnode)
# 2. Node A 挖一些区块
# 3. 启动 Node B (连接 Node A，同步)
# 4. 断开 Node B，让 Node B 独立挖矿 (分叉)
# 5. 重新连接，观察 reorg

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${ROOT_DIR}/target/release/rabbitchain"
TEST_DIR="${ROOT_DIR}/target/test-data"
LOG_DIR="${TEST_DIR}/logs-fork"
AUTH_TOKEN="rabbit-fork-test-token"
COINBASE="0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9"

mkdir -p "${LOG_DIR}"

cleanup() {
    echo ""; echo "=== Cleaning up ==="
    pkill -f "rabbitchain" 2>/dev/null || true
    for f in "${LOG_DIR}"/*.pid; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null || true; done
    wait 2>/dev/null || true; echo "=== Cleanup complete ==="
}
trap cleanup EXIT INT TERM

rpc_call() {
    local url="$1" method="$2" params="${3:-[]}" token="${4:-$AUTH_TOKEN}"
    local payload; payload="$(printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "${method}" "${params}")"
    local args=(-fsS --max-time 5 -H 'Content-Type: application/json' --data "${payload}")
    [ -n "${token}" ] && args+=(-H "authorization: Bearer ${token}" -H "x-rabbit-token: ${token}")
    curl "${args[@]}" "${url}" 2>/dev/null || echo '{"error":"curl_failed"}'
}

get_latest_info() {
    local url="$1" token="${2:-$AUTH_TOKEN}"
    local json; json="$(rpc_call "${url}" "rabbit_getLatestBlock" '[]' "${token}")"
    local height_hex hash; height_hex="$(echo "${json}" | jq -r '.result.body.number // "0x0"' 2>/dev/null)"
    hash="$(echo "${json}" | jq -r '.result.hash // "0x0"' 2>/dev/null)"
    local height_dec=$((16#${height_hex#0x}))
    echo "${height_dec}|${hash}"
}

wait_rpc() {
    local port="$1" timeout="${2:-60}" token="${3:-$AUTH_TOKEN}" i=0
    while [ "${i}" -lt "${timeout}" ]; do
        if rpc_call "http://127.0.0.1:${port}" "net_version" '[]' "${token}" 2>/dev/null | grep -q "result"; then
            return 0
        fi
        sleep 1; i=$((i + 1))
    done
    echo "Timeout RPC ${port}" >&2; return 1
}

echo "=============================================="
echo "  RabbitChain 分叉场景测试"
echo "=============================================="

cleanup; sleep 1
rm -rf "${TEST_DIR}/node-a-fork" "${TEST_DIR}/node-b-fork"
mkdir -p "${TEST_DIR}/node-a-fork" "${TEST_DIR}/node-b-fork"

echo "=== Phase 1: Start Node A (boot miner, port 18546) ==="
"${BIN}" --data-dir "${TEST_DIR}/node-a-fork" run \
    --http-port 18546 --ws-port 18547 \
    --mine --coinbase "${COINBASE}" \
    --rpc-auth-token "${AUTH_TOKEN}" --rpc-rate-limit-per-minute 0 \
    --p2p-listen-addr 127.0.0.1 --p2p-listen-port 30304 \
    --disable-p2p-ws --max-peers 10 \
    --p2p-peer-id "node-a" \
    > "${LOG_DIR}/node-a.log" 2>&1 &
echo $! > "${LOG_DIR}/node-a.pid"
wait_rpc 18546 60; echo "Node A ready"

echo "Letting Node A mine to height 5+..."
for i in $(seq 1 60); do
    info=$(get_latest_info "http://127.0.0.1:18546")
    h=$(echo "${info}" | cut -d'|' -f1)
    echo "[${i}s] A=${h}"
    [ "${h}" -ge 5 ] 2>/dev/null && { echo "✅ A at ${h}"; break; }
    sleep 2
done

echo "=== Phase 2: Start Node B (follower, port 28546) ==="
"${BIN}" --data-dir "${TEST_DIR}/node-b-fork" run \
    --http-port 28546 --ws-port 28547 \
    --rpc-auth-token "${AUTH_TOKEN}" --rpc-rate-limit-per-minute 0 \
    --p2p-listen-addr 127.0.0.1 --p2p-listen-port 31304 \
    --bootnode "enode://node-a@127.0.0.1:30304" \
    --disable-p2p-ws --max-peers 10 \
    --p2p-peer-id "node-b" \
    > "${LOG_DIR}/node-b.log" 2>&1 &
echo $! > "${LOG_DIR}/node-b.pid"
wait_rpc 28546 60; echo "Node B ready"

echo "Waiting for Node B to sync..."
for i in $(seq 1 30); do
    info_a=$(get_latest_info "http://127.0.0.1:18546")
    info_b=$(get_latest_info "http://127.0.0.1:28546")
    ha=$(echo "${info_a}" | cut -d'|' -f1)
    hb=$(echo "${info_b}" | cut -d'|' -f1)
    echo "A=${ha} B=${hb}"
    [ $((ha - hb)) -le 1 ] 2>/dev/null && { echo "✅ B synced"; break; }
    sleep 3
done

echo "=== Phase 3: Fork - disconnect B and mine independently ==="
kill "$(cat "${LOG_DIR}/node-b.pid")" 2>/dev/null || true
sleep 3; echo "Node B stopped"

echo "Starting Node B as standalone miner..."
"${BIN}" --data-dir "${TEST_DIR}/node-b-fork" run \
    --http-port 28546 --ws-port 28547 \
    --mine --coinbase "${COINBASE}" \
    --rpc-auth-token "${AUTH_TOKEN}" --rpc-rate-limit-per-minute 0 \
    --p2p-listen-addr 127.0.0.1 --p2p-listen-port 31304 \
    --disable-p2p-ws --max-peers 10 \
    --p2p-peer-id "node-b" \
    > "${LOG_DIR}/node-b-fork.log" 2>&1 &
echo $! > "${LOG_DIR}/node-b-fork.pid"
wait_rpc 28546 60; echo "Node B (fork) ready"

echo "Letting Node B mine ahead..."
for i in $(seq 1 60); do
    info_b=$(get_latest_info "http://127.0.0.1:28546")
    info_a=$(get_latest_info "http://127.0.0.1:18546")
    hb=$(echo "${info_b}" | cut -d'|' -f1)
    ha=$(echo "${info_a}" | cut -d'|' -f1)
    echo "A=${ha} B=${hb}"
    [ "${hb}" -ge 8 ] 2>/dev/null && { echo "✅ B at ${hb}"; break; }
    sleep 2
done

echo "=== Phase 4: Compare chains ==="
info_a=$(get_latest_info "http://127.0.0.1:18546")
info_b=$(get_latest_info "http://127.0.0.1:28546")
ha=$(echo "${info_a}" | cut -d'|' -f1)
hb=$(echo "${info_b}" | cut -d'|' -f1)
echo "A height=${ha}  B height=${hb}"
common=$(( ha < hb ? ha : hb ))
echo "Comparing 0..${common}..."
for h in $(seq 0 "${common}"); do
    json_a=$(rpc_call "http://127.0.0.1:18546" "rabbit_getBlockByNumber" "[${h}]")
    json_b=$(rpc_call "http://127.0.0.1:28546" "rabbit_getBlockByNumber" "[${h}]")
    ha_a=$(echo "${json_a}" | jq -r '.result.hash // "MISSING"' 2>/dev/null)
    ha_b=$(echo "${json_b}" | jq -r '.result.hash // "MISSING"' 2>/dev/null)
    if [ "${ha_a}" != "${ha_b}" ]; then
        echo "❌ FORK at ${h}: A=${ha_a:0:24} B=${ha_b:0:24}"
        # Record first fork height for later verification
        [ -z "${first_fork_h+set}" ] && first_fork_h=$h
    fi
done

echo "=== Phase 5: Reconnect B to A (observe reorg) ==="
kill "$(cat "${LOG_DIR}/node-b-fork.pid")" 2>/dev/null || true
sleep 3

echo "Restarting Node B connected to bootnode..."
"${BIN}" --data-dir "${TEST_DIR}/node-b-fork" run \
    --http-port 28546 --ws-port 28547 \
    --rpc-auth-token "${AUTH_TOKEN}" --rpc-rate-limit-per-minute 0 \
    --p2p-listen-addr 127.0.0.1 --p2p-listen-port 31304 \
    --bootnode "enode://node-a@127.0.0.1:30304" \
    --disable-p2p-ws --max-peers 10 \
    --p2p-peer-id "node-b" \
    > "${LOG_DIR}/node-b-reconnect.log" 2>&1 &
echo $! > "${LOG_DIR}/node-b-reconnect.pid"
wait_rpc 28546 60; echo "Node B reconnected!"

echo "Monitoring re-sync (should reorg to follow A)..."
for i in $(seq 1 30); do
    info_a=$(get_latest_info "http://127.0.0.1:18546")
    info_b=$(get_latest_info "http://127.0.0.1:28546")
    ha=$(echo "${info_a}" | cut -d'|' -f1)
    hb=$(echo "${info_b}" | cut -d'|' -f1)
    echo "A=${ha} B=${hb} gap=$((ha - hb))"
    
    if [ $((ha - hb)) -le 1 ] 2>/dev/null; then
        json_a=$(rpc_call "http://127.0.0.1:18546" "rabbit_getBlockByNumber" "[${hb}]")
        json_b=$(rpc_call "http://127.0.0.1:28546" "rabbit_getBlockByNumber" "[${hb}]")
        ha_a=$(echo "${json_a}" | jq -r '.result.hash // "MISSING"' 2>/dev/null)
        ha_b=$(echo "${json_b}" | jq -r '.result.hash // "MISSING"' 2>/dev/null)
        if [ "${ha_a}" = "${ha_b}" ]; then
            echo "✅ Node B reorged to A's chain at height ${hb}"
        else
            echo "❌ Still forked at height ${hb}"
        fi
        break
    fi
    sleep 3
done

echo ""
echo "=============================================="
echo "  FORK TEST COMPLETE"
echo "=============================================="
echo "Logs: ${LOG_DIR}"
