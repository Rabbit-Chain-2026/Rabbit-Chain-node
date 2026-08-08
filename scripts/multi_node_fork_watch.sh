#!/usr/bin/env bash

set -euo pipefail

VPS_RPC_URL="${VPS_RPC_URL:-http://127.0.0.1:29545}"
VPS_RPC_TOKEN="${VPS_RPC_TOKEN:-rabbit-mainnet-miner}"
FOLLOWER_RPC_URL="${FOLLOWER_RPC_URL:-http://127.0.0.1:39645}"
OBSERVER_RPC_URL="${OBSERVER_RPC_URL:-http://127.0.0.1:39745}"
SAMPLES="${SAMPLES:-5}"
INTERVAL_SECS="${INTERVAL_SECS:-15}"

rpc_call() {
    local url="$1"
    local method="$2"
    local params="$3"
    local token="${4:-}"
    local payload
    payload="$(printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "${method}" "${params}")"

    local args=(
        -fsS
        --max-time 8
        -H 'Content-Type: application/json'
        --data "${payload}"
    )
    if [[ -n "${token}" ]]; then
        args+=(-H "authorization: Bearer ${token}" -H "x-rabbit-token: ${token}")
    fi
    curl "${args[@]}" "${url}"
}

latest_info() {
    local name="$1"
    local url="$2"
    local token="${3:-}"
    local json
    json="$(rpc_call "${url}" "rabbit_getLatestBlock" '[]' "${token}")"
    jq -r --arg name "${name}" '
      .result as $r
      | "\($name)\t\($r.body.number)\t\($r.hash)\t\($r.body.block_hash)"
    ' <<<"${json}"
}

block_hash_at() {
    local url="$1"
    local height="$2"
    local token="${3:-}"
    local json
    json="$(rpc_call "${url}" "rabbit_getBlockByNumber" "[${height}]" "${token}")"
    jq -r '.result.hash' <<<"${json}"
}

hex_to_dec() {
    local value="$1"
    printf '%d' "$((16#${value#0x}))"
}

min3() {
    local a="$1"
    local b="$2"
    local c="$3"
    if (( a <= b && a <= c )); then
        echo "${a}"
    elif (( b <= a && b <= c )); then
        echo "${b}"
    else
        echo "${c}"
    fi
}

echo "watch_start $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "vps=${VPS_RPC_URL}"
echo "follower=${FOLLOWER_RPC_URL}"
echo "observer=${OBSERVER_RPC_URL}"

failures=0
sample=1
while (( sample <= SAMPLES )); do
    vps_line="$(latest_info vps "${VPS_RPC_URL}" "${VPS_RPC_TOKEN}")"
    follower_line="$(latest_info follower "${FOLLOWER_RPC_URL}")"
    observer_line="$(latest_info observer "${OBSERVER_RPC_URL}")"

    vps_height_hex="$(awk -F'\t' '{print $2}' <<<"${vps_line}")"
    follower_height_hex="$(awk -F'\t' '{print $2}' <<<"${follower_line}")"
    observer_height_hex="$(awk -F'\t' '{print $2}' <<<"${observer_line}")"

    vps_height="$(hex_to_dec "${vps_height_hex}")"
    follower_height="$(hex_to_dec "${follower_height_hex}")"
    observer_height="$(hex_to_dec "${observer_height_hex}")"
    common_height="$(min3 "${vps_height}" "${follower_height}" "${observer_height}")"

    vps_common_hash="$(block_hash_at "${VPS_RPC_URL}" "${common_height}" "${VPS_RPC_TOKEN}")"
    follower_common_hash="$(block_hash_at "${FOLLOWER_RPC_URL}" "${common_height}")"
    observer_common_hash="$(block_hash_at "${OBSERVER_RPC_URL}" "${common_height}")"

    echo "sample=${sample}"
    echo "  latest vps=${vps_height_hex} follower=${follower_height_hex} observer=${observer_height_hex}"
    echo "  common_height=${common_height}"
    echo "  common_hash vps=${vps_common_hash} follower=${follower_common_hash} observer=${observer_common_hash}"

    if [[ "${vps_common_hash}" == "${follower_common_hash}" && "${vps_common_hash}" == "${observer_common_hash}" ]]; then
        echo "  status=shared_chain"
    else
        echo "  status=fork_detected"
        failures=$((failures + 1))
    fi

    if (( sample < SAMPLES )); then
        sleep "${INTERVAL_SECS}"
    fi
    sample=$((sample + 1))
done

echo "watch_end failures=${failures}"
if (( failures > 0 )); then
    exit 1
fi
