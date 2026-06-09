#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUN_ROOT="${ROOT_DIR}/artifacts/public-node-soak-monitor"
CURRENT_ENV="${RUN_ROOT}/current.env"
PID_FILE="${RUN_ROOT}/monitor.pid"

DEFAULT_DURATION_SECS=$((72 * 3600))
DEFAULT_INTERVAL_SECS=60
DEFAULT_RPC_TIMEOUT_SECS=8
DEFAULT_SSH_TIMEOUT_SECS=8
DEFAULT_RPC_RETRIES=3
DEFAULT_LOCAL_RPC_URL="http://127.0.0.1:29645"
DEFAULT_REMOTE_HOST="139.180.207.66"
DEFAULT_REMOTE_USER="root"
DEFAULT_REMOTE_RPC_PORT=28545
DEFAULT_SSH_KEY="${HOME}/.ssh/agent_139_180_207_66"
DEFAULT_LOCAL_SOAK_ENV="${ROOT_DIR}/artifacts/public-node-soak/current.env"
DEFAULT_REMOTE_SOAK_ENV="/root/works/Rabbit-Chain-node-public-soak/current.env"

usage() {
    cat <<'EOF'
Usage:
  scripts/public_node_soak_monitor.sh start [options]
  scripts/public_node_soak_monitor.sh stop
  scripts/public_node_soak_monitor.sh status
  scripts/public_node_soak_monitor.sh logs

Options:
  --duration-secs <n>       Monitoring duration in seconds (default: 259200 / 72h)
  --duration-hours <n>      Monitoring duration in hours (overrides --duration-secs)
  --interval-secs <n>       Sampling interval in seconds (default: 60)
  --local-rpc-url <url>     Local node RPC URL (default: http://127.0.0.1:29645)
  --remote-host <host>      Remote SSH host (default: 139.180.207.66)
  --remote-user <user>      Remote SSH user (default: root)
  --remote-rpc-port <port>  Remote node local RPC port (default: 28545)
  --ssh-key <path>          SSH private key path
  --local-node-pid <pid>    Local public node PID override
  --remote-node-pid <pid>   Remote public node PID override
  --local-soak-env <path>   Local node env file (default: artifacts/public-node-soak/current.env)
  --remote-soak-env <path>  Remote node env file (default: /root/works/Rabbit-Chain-node-public-soak/current.env)
  --rpc-timeout-secs <n>    curl timeout per RPC request (default: 8)
  --rpc-retries <n>         RPC retries before marking sample failed (default: 3)
  --ssh-timeout-secs <n>    SSH connect timeout (default: 8)
EOF
}

require_cmd() {
    local cmd="$1"
    if ! command -v "${cmd}" >/dev/null 2>&1; then
        echo "Missing command: ${cmd}" >&2
        exit 1
    fi
}

is_running_pid() {
    local pid="$1"
    kill -0 "${pid}" 2>/dev/null
}

timestamp_compact() {
    date -u +%Y%m%dT%H%M%SZ
}

timestamp_iso() {
    date -u +%Y-%m-%dT%H:%M:%SZ
}

write_key_values_file() {
    local file="$1"
    shift
    local tmp="${file}.tmp"
    : > "${tmp}"
    while [[ $# -gt 1 ]]; do
        printf '%s=%s\n' "$1" "$2" >> "${tmp}"
        shift 2
    done
    mv "${tmp}" "${file}"
}

json_rpc_payload() {
    local method="$1"
    local params_json="$2"
    printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "${method}" "${params_json}"
}

rpc_call_local() {
    local rpc_url="$1"
    local method="$2"
    local params_json="$3"
    local timeout_secs="$4"
    local payload
    payload="$(json_rpc_payload "${method}" "${params_json}")"
    curl -fsS --max-time "${timeout_secs}" \
        -H 'content-type: application/json' \
        --data "${payload}" \
        "${rpc_url}"
}

ssh_exec() {
    local ssh_key="$1"
    local ssh_timeout_secs="$2"
    local remote_user="$3"
    local remote_host="$4"
    local remote_cmd="$5"
    ssh -i "${ssh_key}" \
        -o BatchMode=yes \
        -o ConnectTimeout="${ssh_timeout_secs}" \
        -o StrictHostKeyChecking=no \
        -o UserKnownHostsFile=/dev/null \
        "${remote_user}@${remote_host}" \
        "${remote_cmd}"
}

rpc_call_remote() {
    local ssh_key="$1"
    local ssh_timeout_secs="$2"
    local remote_user="$3"
    local remote_host="$4"
    local remote_rpc_port="$5"
    local method="$6"
    local params_json="$7"
    local rpc_timeout_secs="$8"
    local payload
    payload="$(json_rpc_payload "${method}" "${params_json}")"
    ssh_exec "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" \
        "curl -fsS --max-time ${rpc_timeout_secs} -H 'content-type: application/json' --data '${payload}' 'http://127.0.0.1:${remote_rpc_port}'"
}

rpc_call_local_with_retries() {
    local rpc_url="$1"
    local method="$2"
    local params_json="$3"
    local timeout_secs="$4"
    local retries="$5"
    local attempt=1
    local output=""
    while (( attempt <= retries )); do
        if output="$(rpc_call_local "${rpc_url}" "${method}" "${params_json}" "${timeout_secs}" 2>/dev/null)"; then
            printf '%s' "${output}"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    return 1
}

rpc_call_remote_with_retries() {
    local ssh_key="$1"
    local ssh_timeout_secs="$2"
    local remote_user="$3"
    local remote_host="$4"
    local remote_rpc_port="$5"
    local method="$6"
    local params_json="$7"
    local rpc_timeout_secs="$8"
    local retries="$9"
    local attempt=1
    local output=""
    while (( attempt <= retries )); do
        if output="$(rpc_call_remote "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" "${remote_rpc_port}" "${method}" "${params_json}" "${rpc_timeout_secs}" 2>/dev/null)"; then
            printf '%s' "${output}"
            return 0
        fi
        attempt=$((attempt + 1))
        sleep 1
    done
    return 1
}

extract_peer_count() {
    local json="$1"
    if [[ "${json}" != *'"result"'* ]]; then
        echo "-1"
        return
    fi
    local count
    count="$(printf '%s' "${json}" | grep -o '"peer_id"' | wc -l | tr -d ' ')"
    if [[ -z "${count}" ]]; then
        count="0"
    fi
    echo "${count}"
}

discover_local_node_pid() {
    local local_env="$1"
    if [[ ! -f "${local_env}" ]]; then
        return 1
    fi
    # shellcheck disable=SC1090
    source "${local_env}"
    if [[ -n "${NODE_PID:-}" ]]; then
        printf '%s' "${NODE_PID}"
        return 0
    fi
    return 1
}

discover_remote_node_pid() {
    local ssh_key="$1"
    local ssh_timeout_secs="$2"
    local remote_user="$3"
    local remote_host="$4"
    local remote_env="$5"
    local out
    if out="$(ssh_exec "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" \
        "if [ -f '${remote_env}' ]; then . '${remote_env}'; printf '%s' \"\${NODE_PID:-}\"; fi" 2>/dev/null)"; then
        if [[ -n "${out}" ]]; then
            printf '%s' "${out}"
            return 0
        fi
    fi
    return 1
}

remote_pid_alive() {
    local ssh_key="$1"
    local ssh_timeout_secs="$2"
    local remote_user="$3"
    local remote_host="$4"
    local pid="$5"
    if ssh_exec "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" \
        "kill -0 ${pid} 2>/dev/null"; then
        return 0
    fi
    return 1
}

assert_positive_int() {
    local key="$1"
    local value="$2"
    if ! [[ "${value}" =~ ^[0-9]+$ ]]; then
        echo "Invalid ${key}: ${value}" >&2
        exit 1
    fi
}

start_monitor() {
    local duration_secs="${DEFAULT_DURATION_SECS}"
    local interval_secs="${DEFAULT_INTERVAL_SECS}"
    local rpc_timeout_secs="${DEFAULT_RPC_TIMEOUT_SECS}"
    local ssh_timeout_secs="${DEFAULT_SSH_TIMEOUT_SECS}"
    local rpc_retries="${DEFAULT_RPC_RETRIES}"
    local local_rpc_url="${DEFAULT_LOCAL_RPC_URL}"
    local remote_host="${DEFAULT_REMOTE_HOST}"
    local remote_user="${DEFAULT_REMOTE_USER}"
    local remote_rpc_port="${DEFAULT_REMOTE_RPC_PORT}"
    local ssh_key="${DEFAULT_SSH_KEY}"
    local local_node_pid=""
    local remote_node_pid=""
    local local_soak_env="${DEFAULT_LOCAL_SOAK_ENV}"
    local remote_soak_env="${DEFAULT_REMOTE_SOAK_ENV}"

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --duration-secs)
                duration_secs="${2:-}"
                shift 2
                ;;
            --duration-hours)
                assert_positive_int "--duration-hours" "${2:-}"
                duration_secs="$(( ${2} * 3600 ))"
                shift 2
                ;;
            --interval-secs)
                interval_secs="${2:-}"
                shift 2
                ;;
            --local-rpc-url)
                local_rpc_url="${2:-}"
                shift 2
                ;;
            --remote-host)
                remote_host="${2:-}"
                shift 2
                ;;
            --remote-user)
                remote_user="${2:-}"
                shift 2
                ;;
            --remote-rpc-port)
                remote_rpc_port="${2:-}"
                shift 2
                ;;
            --ssh-key)
                ssh_key="${2:-}"
                shift 2
                ;;
            --local-node-pid)
                local_node_pid="${2:-}"
                shift 2
                ;;
            --remote-node-pid)
                remote_node_pid="${2:-}"
                shift 2
                ;;
            --local-soak-env)
                local_soak_env="${2:-}"
                shift 2
                ;;
            --remote-soak-env)
                remote_soak_env="${2:-}"
                shift 2
                ;;
            --rpc-timeout-secs)
                rpc_timeout_secs="${2:-}"
                shift 2
                ;;
            --rpc-retries)
                rpc_retries="${2:-}"
                shift 2
                ;;
            --ssh-timeout-secs)
                ssh_timeout_secs="${2:-}"
                shift 2
                ;;
            *)
                echo "Unknown option: $1" >&2
                usage
                exit 1
                ;;
        esac
    done

    assert_positive_int "--duration-secs" "${duration_secs}"
    assert_positive_int "--interval-secs" "${interval_secs}"
    assert_positive_int "--remote-rpc-port" "${remote_rpc_port}"
    assert_positive_int "--rpc-timeout-secs" "${rpc_timeout_secs}"
    assert_positive_int "--rpc-retries" "${rpc_retries}"
    assert_positive_int "--ssh-timeout-secs" "${ssh_timeout_secs}"

    require_cmd curl
    require_cmd ssh
    require_cmd grep
    require_cmd wc
    require_cmd sed

    if [[ ! -f "${ssh_key}" ]]; then
        echo "SSH key not found: ${ssh_key}" >&2
        exit 1
    fi

    mkdir -p "${RUN_ROOT}"

    if [[ -f "${CURRENT_ENV}" ]]; then
        # shellcheck disable=SC1090
        source "${CURRENT_ENV}" || true
        if [[ -n "${MONITOR_PID:-}" ]] && is_running_pid "${MONITOR_PID}"; then
            echo "public-node-soak monitor already running pid=${MONITOR_PID}" >&2
            echo "run_dir=${RUN_DIR:-unknown}" >&2
            exit 1
        fi
    fi

    if [[ -z "${local_node_pid}" ]]; then
        local_node_pid="$(discover_local_node_pid "${local_soak_env}" || true)"
    fi
    if [[ -z "${remote_node_pid}" ]]; then
        remote_node_pid="$(discover_remote_node_pid "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" "${remote_soak_env}" || true)"
    fi

    local run_dir="${RUN_ROOT}/$(timestamp_compact)"
    local samples_csv="${run_dir}/samples.csv"
    local status_env="${run_dir}/status.env"
    local summary_txt="${run_dir}/summary.txt"
    local monitor_out="${run_dir}/monitor.out.log"

    mkdir -p "${run_dir}"
    printf '%s\n' \
        "timestamp,sample,elapsed_secs,local_ok,local_peers,remote_ok,remote_peers,local_client_ok,remote_client_ok,local_node_alive,remote_node_alive,local_drop_events,local_recover_events,remote_drop_events,remote_recover_events,local_rpc_errors,remote_rpc_errors,ssh_errors" \
        > "${samples_csv}"

    write_key_values_file "${run_dir}/config.env" \
        RUN_DIR "${run_dir}" \
        STARTED_AT "$(timestamp_iso)" \
        DURATION_SECS "${duration_secs}" \
        INTERVAL_SECS "${interval_secs}" \
        LOCAL_RPC_URL "${local_rpc_url}" \
        REMOTE_HOST "${remote_host}" \
        REMOTE_USER "${remote_user}" \
        REMOTE_RPC_PORT "${remote_rpc_port}" \
        SSH_KEY "${ssh_key}" \
        LOCAL_NODE_PID "${local_node_pid}" \
        REMOTE_NODE_PID "${remote_node_pid}" \
        LOCAL_SOAK_ENV "${local_soak_env}" \
        REMOTE_SOAK_ENV "${remote_soak_env}" \
        RPC_TIMEOUT_SECS "${rpc_timeout_secs}" \
        RPC_RETRIES "${rpc_retries}" \
        SSH_TIMEOUT_SECS "${ssh_timeout_secs}" \
        SAMPLES_CSV "${samples_csv}" \
        STATUS_ENV "${status_env}" \
        SUMMARY_TXT "${summary_txt}" \
        MONITOR_OUT "${monitor_out}"

    if command -v setsid >/dev/null 2>&1; then
        setsid "$0" _run \
            --run-dir "${run_dir}" \
            --duration-secs "${duration_secs}" \
            --interval-secs "${interval_secs}" \
            --local-rpc-url "${local_rpc_url}" \
            --remote-host "${remote_host}" \
            --remote-user "${remote_user}" \
            --remote-rpc-port "${remote_rpc_port}" \
            --ssh-key "${ssh_key}" \
            --local-node-pid "${local_node_pid}" \
            --remote-node-pid "${remote_node_pid}" \
            --rpc-timeout-secs "${rpc_timeout_secs}" \
            --rpc-retries "${rpc_retries}" \
            --ssh-timeout-secs "${ssh_timeout_secs}" \
            >"${monitor_out}" 2>&1 < /dev/null &
    else
        nohup "$0" _run \
            --run-dir "${run_dir}" \
            --duration-secs "${duration_secs}" \
            --interval-secs "${interval_secs}" \
            --local-rpc-url "${local_rpc_url}" \
            --remote-host "${remote_host}" \
            --remote-user "${remote_user}" \
            --remote-rpc-port "${remote_rpc_port}" \
            --ssh-key "${ssh_key}" \
            --local-node-pid "${local_node_pid}" \
            --remote-node-pid "${remote_node_pid}" \
            --rpc-timeout-secs "${rpc_timeout_secs}" \
            --rpc-retries "${rpc_retries}" \
            --ssh-timeout-secs "${ssh_timeout_secs}" \
            >"${monitor_out}" 2>&1 &
    fi

    local monitor_pid=$!
    sleep 1
    if ! is_running_pid "${monitor_pid}"; then
        echo "Failed to start monitor process" >&2
        tail -n 120 "${monitor_out}" >&2 || true
        exit 1
    fi

    write_key_values_file "${CURRENT_ENV}" \
        RUN_DIR "${run_dir}" \
        MONITOR_PID "${monitor_pid}" \
        STARTED_AT "$(timestamp_iso)" \
        DURATION_SECS "${duration_secs}" \
        INTERVAL_SECS "${interval_secs}" \
        LOCAL_RPC_URL "${local_rpc_url}" \
        REMOTE_HOST "${remote_host}" \
        REMOTE_USER "${remote_user}" \
        REMOTE_RPC_PORT "${remote_rpc_port}" \
        SSH_KEY "${ssh_key}" \
        LOCAL_NODE_PID "${local_node_pid}" \
        REMOTE_NODE_PID "${remote_node_pid}" \
        STATUS_ENV "${status_env}" \
        SAMPLES_CSV "${samples_csv}" \
        RPC_RETRIES "${rpc_retries}" \
        MONITOR_OUT "${monitor_out}" \
        SUMMARY_TXT "${summary_txt}"
    echo "${monitor_pid}" > "${PID_FILE}"

    echo "started public-node-soak monitor pid=${monitor_pid}"
    echo "run_dir=${run_dir}"
    echo "samples=${samples_csv}"
}

run_monitor() {
    local run_dir=""
    local duration_secs=""
    local interval_secs=""
    local local_rpc_url=""
    local remote_host=""
    local remote_user=""
    local remote_rpc_port=""
    local ssh_key=""
    local local_node_pid=""
    local remote_node_pid=""
    local rpc_timeout_secs=""
    local rpc_retries=""
    local ssh_timeout_secs=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --run-dir)
                run_dir="${2:-}"
                shift 2
                ;;
            --duration-secs)
                duration_secs="${2:-}"
                shift 2
                ;;
            --interval-secs)
                interval_secs="${2:-}"
                shift 2
                ;;
            --local-rpc-url)
                local_rpc_url="${2:-}"
                shift 2
                ;;
            --remote-host)
                remote_host="${2:-}"
                shift 2
                ;;
            --remote-user)
                remote_user="${2:-}"
                shift 2
                ;;
            --remote-rpc-port)
                remote_rpc_port="${2:-}"
                shift 2
                ;;
            --ssh-key)
                ssh_key="${2:-}"
                shift 2
                ;;
            --local-node-pid)
                local_node_pid="${2:-}"
                shift 2
                ;;
            --remote-node-pid)
                remote_node_pid="${2:-}"
                shift 2
                ;;
            --rpc-timeout-secs)
                rpc_timeout_secs="${2:-}"
                shift 2
                ;;
            --rpc-retries)
                rpc_retries="${2:-}"
                shift 2
                ;;
            --ssh-timeout-secs)
                ssh_timeout_secs="${2:-}"
                shift 2
                ;;
            *)
                echo "Unknown option for _run: $1" >&2
                exit 1
                ;;
        esac
    done

    assert_positive_int "--duration-secs" "${duration_secs}"
    assert_positive_int "--interval-secs" "${interval_secs}"
    assert_positive_int "--remote-rpc-port" "${remote_rpc_port}"
    assert_positive_int "--rpc-timeout-secs" "${rpc_timeout_secs}"
    assert_positive_int "--rpc-retries" "${rpc_retries}"
    assert_positive_int "--ssh-timeout-secs" "${ssh_timeout_secs}"

    local samples_csv="${run_dir}/samples.csv"
    local status_env="${run_dir}/status.env"
    local summary_txt="${run_dir}/summary.txt"

    mkdir -p "${run_dir}"
    if [[ ! -f "${samples_csv}" ]]; then
        printf '%s\n' \
            "timestamp,sample,elapsed_secs,local_ok,local_peers,remote_ok,remote_peers,local_client_ok,remote_client_ok,local_node_alive,remote_node_alive,local_drop_events,local_recover_events,remote_drop_events,remote_recover_events,local_rpc_errors,remote_rpc_errors,ssh_errors" \
            > "${samples_csv}"
    fi

    local start_epoch
    start_epoch="$(date +%s)"
    local end_epoch=$(( start_epoch + duration_secs ))
    local started_at
    started_at="$(timestamp_iso)"

    local sample=0
    local prev_local_peers=-1
    local prev_remote_peers=-1
    local local_drop_events=0
    local local_recover_events=0
    local remote_drop_events=0
    local remote_recover_events=0
    local local_rpc_errors=0
    local remote_rpc_errors=0
    local ssh_errors=0

    cleanup() {
        local ended_at
        ended_at="$(timestamp_iso)"
        write_key_values_file "${summary_txt}" \
            STARTED_AT "${started_at}" \
            ENDED_AT "${ended_at}" \
            TOTAL_SAMPLES "${sample}" \
            LOCAL_DROP_EVENTS "${local_drop_events}" \
            LOCAL_RECOVER_EVENTS "${local_recover_events}" \
            REMOTE_DROP_EVENTS "${remote_drop_events}" \
            REMOTE_RECOVER_EVENTS "${remote_recover_events}" \
            LOCAL_RPC_ERRORS "${local_rpc_errors}" \
            REMOTE_RPC_ERRORS "${remote_rpc_errors}" \
            SSH_ERRORS "${ssh_errors}" \
            DURATION_SECS "$(( $(date +%s) - start_epoch ))"
    }
    trap 'cleanup; exit 0' INT TERM

    while true; do
        local now_epoch
        now_epoch="$(date +%s)"
        if (( now_epoch >= end_epoch )); then
            break
        fi
        local elapsed_secs=$(( now_epoch - start_epoch ))
        local ts
        ts="$(timestamp_iso)"

        local local_ok=0
        local local_peers=-1
        local local_client_ok=0
        local remote_ok=0
        local remote_peers=-1
        local remote_client_ok=0
        local local_node_alive=-1
        local remote_node_alive=-1

        local local_peers_json=""
        if local_peers_json="$(rpc_call_local_with_retries "${local_rpc_url}" "rabbit_peers" "[]" "${rpc_timeout_secs}" "${rpc_retries}")"; then
            local_ok=1
            local_peers="$(extract_peer_count "${local_peers_json}")"
        else
            local_rpc_errors=$((local_rpc_errors + 1))
        fi

        if rpc_call_local_with_retries "${local_rpc_url}" "rabbit_clientVersion" "[]" "${rpc_timeout_secs}" "${rpc_retries}" >/dev/null 2>&1; then
            local_client_ok=1
        fi

        local remote_peers_json=""
        if remote_peers_json="$(rpc_call_remote_with_retries "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" "${remote_rpc_port}" "rabbit_peers" "[]" "${rpc_timeout_secs}" "${rpc_retries}")"; then
            remote_ok=1
            remote_peers="$(extract_peer_count "${remote_peers_json}")"
        else
            remote_rpc_errors=$((remote_rpc_errors + 1))
        fi

        if rpc_call_remote_with_retries "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" "${remote_rpc_port}" "rabbit_clientVersion" "[]" "${rpc_timeout_secs}" "${rpc_retries}" >/dev/null 2>&1; then
            remote_client_ok=1
        else
            # this is optional health data; do not count as hard rpc error
            :
        fi

        if [[ -n "${local_node_pid}" ]]; then
            if is_running_pid "${local_node_pid}"; then
                local_node_alive=1
            else
                local_node_alive=0
            fi
        fi

        if [[ -n "${remote_node_pid}" ]]; then
            if remote_pid_alive "${ssh_key}" "${ssh_timeout_secs}" "${remote_user}" "${remote_host}" "${remote_node_pid}" >/dev/null 2>&1; then
                remote_node_alive=1
            else
                remote_node_alive=0
                ssh_errors=$((ssh_errors + 1))
            fi
        fi

        if (( local_peers >= 0 )); then
            if (( prev_local_peers > 0 && local_peers == 0 )); then
                local_drop_events=$((local_drop_events + 1))
            elif (( prev_local_peers == 0 && local_peers > 0 )); then
                local_recover_events=$((local_recover_events + 1))
            fi
            prev_local_peers="${local_peers}"
        fi

        if (( remote_peers >= 0 )); then
            if (( prev_remote_peers > 0 && remote_peers == 0 )); then
                remote_drop_events=$((remote_drop_events + 1))
            elif (( prev_remote_peers == 0 && remote_peers > 0 )); then
                remote_recover_events=$((remote_recover_events + 1))
            fi
            prev_remote_peers="${remote_peers}"
        fi

        sample=$((sample + 1))
        printf '%s,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d,%d\n' \
            "${ts}" \
            "${sample}" \
            "${elapsed_secs}" \
            "${local_ok}" \
            "${local_peers}" \
            "${remote_ok}" \
            "${remote_peers}" \
            "${local_client_ok}" \
            "${remote_client_ok}" \
            "${local_node_alive}" \
            "${remote_node_alive}" \
            "${local_drop_events}" \
            "${local_recover_events}" \
            "${remote_drop_events}" \
            "${remote_recover_events}" \
            "${local_rpc_errors}" \
            "${remote_rpc_errors}" \
            "${ssh_errors}" \
            >> "${samples_csv}"

        write_key_values_file "${status_env}" \
            LAST_TS "${ts}" \
            SAMPLE "${sample}" \
            ELAPSED_SECS "${elapsed_secs}" \
            LOCAL_OK "${local_ok}" \
            LOCAL_PEERS "${local_peers}" \
            REMOTE_OK "${remote_ok}" \
            REMOTE_PEERS "${remote_peers}" \
            LOCAL_CLIENT_OK "${local_client_ok}" \
            REMOTE_CLIENT_OK "${remote_client_ok}" \
            LOCAL_NODE_ALIVE "${local_node_alive}" \
            REMOTE_NODE_ALIVE "${remote_node_alive}" \
            LOCAL_DROP_EVENTS "${local_drop_events}" \
            LOCAL_RECOVER_EVENTS "${local_recover_events}" \
            REMOTE_DROP_EVENTS "${remote_drop_events}" \
            REMOTE_RECOVER_EVENTS "${remote_recover_events}" \
            LOCAL_RPC_ERRORS "${local_rpc_errors}" \
            REMOTE_RPC_ERRORS "${remote_rpc_errors}" \
            SSH_ERRORS "${ssh_errors}"

        echo "${ts} sample=${sample} local_peers=${local_peers} remote_peers=${remote_peers} local_ok=${local_ok} remote_ok=${remote_ok} local_drop=${local_drop_events} remote_drop=${remote_drop_events} local_rpc_errors=${local_rpc_errors} remote_rpc_errors=${remote_rpc_errors} ssh_errors=${ssh_errors}"

        sleep "${interval_secs}"
    done

    cleanup
    trap - INT TERM
}

stop_monitor() {
    if [[ ! -f "${CURRENT_ENV}" ]]; then
        echo "public-node-soak monitor not running"
        return 0
    fi

    # shellcheck disable=SC1090
    source "${CURRENT_ENV}" || true
    local pid="${MONITOR_PID:-}"
    if [[ -z "${pid}" ]]; then
        echo "monitor pid missing in ${CURRENT_ENV}" >&2
        return 1
    fi

    if is_running_pid "${pid}"; then
        kill "${pid}" 2>/dev/null || true
        for _ in {1..40}; do
            if ! is_running_pid "${pid}"; then
                break
            fi
            sleep 0.1
        done
        if is_running_pid "${pid}"; then
            kill -9 "${pid}" 2>/dev/null || true
        fi
        echo "stopped public-node-soak monitor pid=${pid}"
    else
        echo "public-node-soak monitor already stopped (stale pid=${pid})"
    fi
    rm -f "${PID_FILE}"
}

status_monitor() {
    if [[ ! -f "${CURRENT_ENV}" ]]; then
        echo "public-node-soak monitor never started"
        return 0
    fi

    # shellcheck disable=SC1090
    source "${CURRENT_ENV}" || true
    local pid="${MONITOR_PID:-}"
    local run_dir="${RUN_DIR:-}"
    local status_env="${STATUS_ENV:-}"
    local samples_csv="${SAMPLES_CSV:-}"

    if [[ -n "${pid}" ]] && is_running_pid "${pid}"; then
        echo "public-node-soak monitor running pid=${pid}"
    else
        echo "public-node-soak monitor stopped"
    fi

    if [[ -n "${run_dir}" ]]; then
        echo "run_dir=${run_dir}"
    fi
    if [[ -n "${samples_csv}" ]] && [[ -f "${samples_csv}" ]]; then
        echo "samples_file=${samples_csv}"
        echo "last_sample:"
        tail -n 1 "${samples_csv}"
    fi
    if [[ -n "${status_env}" ]] && [[ -f "${status_env}" ]]; then
        echo "status:"
        cat "${status_env}"
    fi
}

logs_monitor() {
    if [[ ! -f "${CURRENT_ENV}" ]]; then
        echo "public-node-soak monitor never started"
        return 0
    fi
    # shellcheck disable=SC1090
    source "${CURRENT_ENV}" || true
    if [[ -n "${MONITOR_OUT:-}" ]] && [[ -f "${MONITOR_OUT}" ]]; then
        echo "== monitor.out.log (tail 80) =="
        tail -n 80 "${MONITOR_OUT}"
    fi
    if [[ -n "${SAMPLES_CSV:-}" ]] && [[ -f "${SAMPLES_CSV}" ]]; then
        echo
        echo "== samples.csv (tail 20) =="
        tail -n 20 "${SAMPLES_CSV}"
    fi
}

cmd="${1:-status}"
shift || true

case "${cmd}" in
    start)
        start_monitor "$@"
        ;;
    _run)
        run_monitor "$@"
        ;;
    stop)
        stop_monitor
        ;;
    status)
        status_monitor
        ;;
    logs)
        logs_monitor
        ;;
    help|-h|--help)
        usage
        ;;
    *)
        echo "Unknown command: ${cmd}" >&2
        usage
        exit 1
        ;;
esac
