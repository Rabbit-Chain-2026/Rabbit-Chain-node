#!/usr/bin/env bash
# RabbitChain 确定性分叉测试
# 独立启动两个矿工从创世块分叉，然后连接观察 reorg
# 使用 --mining-work-target-leading-rabbit-bytes 0 让挖矿足够简单

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${ROOT_DIR}/target/release/rabbitchain"
TEST_DIR="${ROOT_DIR}/target/test-data"
LOG_DIR="${TEST_DIR}/logs-fork-race"
AUTH_TOKEN="rabbit-fork-race-token"
COINBASE="0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9"

mkdir -p "${LOG_DIR}"

cleanup() {
    echo ""; echo "=== Cleanup ==="
    pkill -f "rabbitchain" 2>/dev/null || true
    for f in "${LOG_DIR}"/*.pid; do [ -f "$f" ] && kill "$(cat "$f")" 2>/dev/null || true; done
    wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

rpc_call() {
    local url="$1" method="$2" params="${3:-[]}" token="${4:-$AUTH_TOKEN}"
    local payload; payload="$(printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "${method}" "${params}")"
    local args=(-fsS --max-time 5 -H 'Content-Type: application/json' --data "${payload}")
    [ -n "${token}" ] && args+=(-H "authorization: Bearer ${token}" -H "x-rabbit-token: ${token}")
    curl "${args[@]}" "${url}" 2>/dev/null || echo '{"error":"curl_failed"}'
}

get_height() {
    local url="$1" token="${2:-$AUTH_TOKEN}"
    local json; json="$(rpc_call "${url}" "rabbit_getLatestBlock" '[]' "${token}")"
    local hx; hx="$(echo "${json}" | jq -r '.result.body.number // "0x0"' 2>/dev/null)"
    echo $((16#${hx#0x}))
}

get_hash_at() {
    local url="$1" h="$2" token="${3:-$AUTH_TOKEN}"
    local json; json="$(rpc_call "${url}" "rabbit_getBlockByNumber" "[${h}]" "${token}")"
    echo "${json}" | jq -r '.result.hash // "MISSING"' 2>/dev/null
}

wait_rpc() {
    local port="$1" timeout="${2:-60}" i=0
    while [ "${i}" -lt "${timeout}" ]; do
        if rpc_call "http://127.0.0.1:${port}" "net_version" '[]' 2>/dev/null | grep -q "result"; then
            return 0
        fi
        sleep 1; i=$((i + 1))
    done
    echo "Timeout RPC ${port}" >&2; return 1
}

# Common flags for easy mining
EASY_MINE="--mine --coinbase ${COINBASE} --rpc-auth-token ${AUTH_TOKEN} --rpc-rate-limit-per-minute 0 --p2p-listen-addr 127.0.0.1 --disable-p2p-ws --max-peers 5 --mining-work-target-leading-rabbit-bytes 1"

echo "=============================================="
echo "  RabbitChain 确定性分叉测试"
echo "=============================================="

cleanup; sleep 2
rm -rf "${TEST_DIR}/node-x" "${TEST_DIR}/node-y" "${LOG_DIR}"
mkdir -p "${TEST_DIR}/node-x" "${TEST_DIR}/node-y" "${LOG_DIR}"

# Step 1: Start Node X (miner, standalone) - let it mine blocks from genesis
echo ""
echo "=== Step 1: Start Node X (standalone mining, port 18700) ==="
"${BIN}" --data-dir "${TEST_DIR}/node-x" run \
    --http-port 18700 --ws-port 18701 \
    ${EASY_MINE} \
    --p2p-listen-port 30700 \
    --p2p-peer-id "node-x" \
    > "${LOG_DIR}/node-x.log" 2>&1 &
echo $! > "${LOG_DIR}/node-x.pid"
wait_rpc 18700 60; echo "Node X ready"

echo "Letting Node X mine to height 5+..."
for i in $(seq 1 60); do
    h=$(get_height "http://127.0.0.1:18700")
    echo "[${i}s] X=${h}"
    [ "${h}" -ge 5 ] 2>/dev/null && { echo "✅ X at ${h}"; break; }
    sleep 2
done

# Record X's chain hashes
echo "Recording Node X chain..."
HX=$(get_height "http://127.0.0.1:18700")
for h in $(seq 0 "${HX}"); do
    echo "X block ${h}: $(get_hash_at "http://127.0.0.1:18700" "${h}")"
done

# Stop X
kill "$(cat "${LOG_DIR}/node-x.pid")" 2>/dev/null || true; sleep 2

# Step 2: Start Node Y (miner, standalone, DIFFERENT chain from genesis)
echo ""
echo "=== Step 2: Start Node Y (standalone mining, port 18800) ==="
"${BIN}" --data-dir "${TEST_DIR}/node-y" run \
    --http-port 18800 --ws-port 18801 \
    ${EASY_MINE} \
    --p2p-listen-port 30800 \
    --p2p-peer-id "node-y" \
    > "${LOG_DIR}/node-y.log" 2>&1 &
echo $! > "${LOG_DIR}/node-y.pid"
wait_rpc 18800 60; echo "Node Y ready"

echo "Letting Node Y mine to height 5+..."
for i in $(seq 1 60); do
    h=$(get_height "http://127.0.0.1:18800")
    echo "[${i}s] Y=${h}"
    [ "${h}" -ge 5 ] 2>/dev/null && { echo "✅ Y at ${h}"; break; }
    sleep 2
done

# Record Y's chain hashes
echo "Recording Node Y chain..."
HY=$(get_height "http://127.0.0.1:18800")
for h in $(seq 0 "${HY}"); do
    echo "Y block ${h}: $(get_hash_at "http://127.0.0.1:18800" "${h}")"
done

# Stop Y
kill "$(cat "${LOG_DIR}/node-y.pid")" 2>/dev/null || true; sleep 2

echo ""
echo "=== Step 3: Compare chains ==="
echo "X height=${HX}  Y height=${HY}"
MIN=$(( HX < HY ? HX : HY ))
FORKS=0
for h in $(seq 0 "${MIN}"); do
    HASH_X=$(get_hash_at "http://127.0.0.1:18700" "${h}" 2>/dev/null || echo "DEAD")
    HASH_Y=$(get_hash_at "http://127.0.0.1:18800" "${h}" 2>/dev/null || echo "DEAD")
    # If the RPC is dead, read from the persisted state
    if [ -z "${HASH_X}" ] || [ "${HASH_X}" = "DEAD" ]; then
        HASH_X="N/A"
    fi
    if [ -z "${HASH_Y}" ] || [ "${HASH_Y}" = "DEAD" ]; then
        HASH_Y="N/A"
    fi
    if [ "${h}" -eq 0 ]; then
        echo "  Genesis: X=${HASH_X:0:24} Y=${HASH_Y:0:24}"
    elif [ "${HASH_X}" != "${HASH_Y}" ]; then
        echo "  ❌ FORK at ${h}: X=${HASH_X:0:24} Y=${HASH_Y:0:24}"
        FORKS=$((FORKS + 1))
    else
        echo "  ✅ Block ${h}: X=${HASH_X:0:24} Y=${HASH_Y:0:24}"
    fi
done

echo ""
echo "=== Step 4: Restart Node Y connected to Node X ==="
# But first we need to restart Node X too
echo "Restarting Node X (as bootnode)..."
"${BIN}" --data-dir "${TEST_DIR}/node-x" run \
    --http-port 18700 --ws-port 18701 \
    ${EASY_MINE} \
    --p2p-listen-port 30700 \
    --p2p-peer-id "node-x" \
    > "${LOG_DIR}/node-x-boot.log" 2>&1 &
echo $! > "${LOG_DIR}/node-x-boot.pid"
wait_rpc 18700 60; echo "Node X back online"

echo "Restarting Node Y (connected to X)..."
"${BIN}" --data-dir "${TEST_DIR}/node-y" run \
    --http-port 18800 --ws-port 18801 \
    --rpc-auth-token "${AUTH_TOKEN}" --rpc-rate-limit-per-minute 0 \
    --p2p-listen-addr 127.0.0.1 --p2p-listen-port 30800 \
    --bootnode "enode://node-x@127.0.0.1:30700" \
    --disable-p2p-ws --max-peers 5 \
    --p2p-peer-id "node-y" \
    > "${LOG_DIR}/node-y-reconnect.log" 2>&1 &
echo $! > "${LOG_DIR}/node-y-reconnect.pid"
wait_rpc 18800 60; echo "Node Y reconnected!"

echo "Monitoring re-sync..."
for i in $(seq 1 30); do
    HX=$(get_height "http://127.0.0.1:18700")
    HY=$(get_height "http://127.0.0.1:18800")
    echo "X=${HX} Y=${HY}"
    if [ "${HY}" -ge 1 ] 2>/dev/null; then
        # Check if Y reorged to X's chain at height 1
        HASH_X1=$(get_hash_at "http://127.0.0.1:18700" 1)
        HASH_Y1=$(get_hash_at "http://127.0.0.1:18800" 1)
        if [ "${HASH_X1}" = "${HASH_Y1}" ]; then
            echo "✅ Node Y has REORGED to X's chain! Block 1 matches."
        else
            echo "❌ Block 1 still diverged: X=${HASH_X1:0:24} Y=${HASH_Y1:0:24}"
        fi
        break
    fi
    sleep 3
done

# Final full comparison
echo ""
echo "=== Final Chain Comparison ==="
HX=$(get_height "http://127.0.0.1:18700")
HY=$(get_height "http://127.0.0.1:18800")
echo "X=${HX} Y=${HY}"
MIN=$(( HX < HY ? HX : HY ))
FORKS=0
for h in $(seq 0 "${MIN}"); do
    HASH_X=$(get_hash_at "http://127.0.0.1:18700" "${h}")
    HASH_Y=$(get_hash_at "http://127.0.0.1:18800" "${h}")
    if [ "${HASH_X}" != "${HASH_Y}" ]; then
        echo "❌ FORK at ${h}: X=${HASH_X:0:24} Y=${HASH_Y:0:24}"
        FORKS=$((FORKS + 1))
    fi
done
if [ "${FORKS}" -eq 0 ]; then
    echo "✅ All blocks consistent - reorg successful!"
else
    echo "⚠️ ${FORKS} forks remain"
fi

echo ""
echo "=============================================="
echo "  FORK RACE TEST COMPLETE"
echo "=============================================="
echo "Logs: ${LOG_DIR}/"
