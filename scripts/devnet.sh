#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${ROOT_DIR}/target/release/rabbitchain"
DATA_DIR="${HOME}/.rabbitchain/devnet"
LOG_FILE="${DATA_DIR}/devnet.log"
PID_FILE="${DATA_DIR}/devnet.pid"

HTTP_PORT=28545
WS_PORT=28546

usage() {
    cat <<'EOF'
Usage:
  scripts/devnet.sh start [--mine] [--coinbase 0x...] [--clean-data]
  scripts/devnet.sh stop
  scripts/devnet.sh status
  scripts/devnet.sh logs
EOF
}

ensure_binary() {
    if [[ -x "${BIN}" ]]; then
        return 0
    fi
    if command -v cargo >/dev/null 2>&1; then
        (cd "${ROOT_DIR}" && cargo build -p rabbitcli --release)
    else
        echo "missing binary and cargo"
        exit 1
    fi
}

is_running_pid() {
    local pid="$1"
    kill -0 "${pid}" 2>/dev/null
}

start_node() {
    local mine="$1"
    local coinbase="$2"
    local clean_data="$3"

    ensure_binary
    mkdir -p "${DATA_DIR}"

    if [[ -f "${PID_FILE}" ]]; then
        local old
        old="$(cat "${PID_FILE}" 2>/dev/null || true)"
        if [[ -n "${old}" ]] && is_running_pid "${old}"; then
            echo "devnet already running pid=${old}"
            exit 1
        fi
        rm -f "${PID_FILE}"
    fi

    if [[ "${clean_data}" == "true" ]]; then
        rm -rf "${DATA_DIR}"
        mkdir -p "${DATA_DIR}"
    fi

    local args=(
        -d "${DATA_DIR}"
        --network devnet
        run
        --http-port "${HTTP_PORT}"
        --ws-port "${WS_PORT}"
    )

    if [[ "${mine}" == "true" ]]; then
        args+=(--mine)
    fi
    if [[ -n "${coinbase}" ]]; then
        args+=(--coinbase "${coinbase}" --rpc-coinbase "${coinbase}")
    fi

    nohup "${BIN}" "${args[@]}" >"${LOG_FILE}" 2>&1 &
    local pid=$!
    echo "${pid}" > "${PID_FILE}"
    sleep 1
    if is_running_pid "${pid}"; then
        echo "started devnet pid=${pid}"
    else
        tail -n 80 "${LOG_FILE}" || true
        rm -f "${PID_FILE}"
        exit 1
    fi
}

stop_node() {
    if [[ ! -f "${PID_FILE}" ]]; then
        echo "devnet not running"
        return 0
    fi
    local pid
    pid="$(cat "${PID_FILE}" 2>/dev/null || true)"
    if [[ -n "${pid}" ]] && is_running_pid "${pid}"; then
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
    fi
    rm -f "${PID_FILE}"
    echo "stopped devnet"
}

status_node() {
    if [[ -f "${PID_FILE}" ]]; then
        local pid
        pid="$(cat "${PID_FILE}" 2>/dev/null || true)"
        if [[ -n "${pid}" ]] && is_running_pid "${pid}"; then
            echo "devnet running pid=${pid}"
            return 0
        fi
    fi
    echo "devnet stopped"
}

show_logs() {
    if [[ -f "${LOG_FILE}" ]]; then
        tail -n 120 "${LOG_FILE}"
    else
        echo "no log file"
    fi
}

cmd="${1:-start}"
shift || true

case "${cmd}" in
    start)
        mine="false"
        coinbase=""
        clean_data="false"
        while [[ $# -gt 0 ]]; do
            case "$1" in
                --mine)
                    mine="true"
                    shift
                    ;;
                --coinbase)
                    coinbase="${2:-}"
                    shift 2
                    ;;
                --clean-data)
                    clean_data="true"
                    shift
                    ;;
                *)
                    echo "unknown option: $1"
                    usage
                    exit 1
                    ;;
            esac
        done
        start_node "${mine}" "${coinbase}" "${clean_data}"
        ;;
    stop)
        stop_node
        ;;
    status)
        status_node
        ;;
    logs)
        show_logs
        ;;
    help|-h|--help)
        usage
        ;;
    *)
        echo "unknown command: ${cmd}"
        usage
        exit 1
        ;;
esac
