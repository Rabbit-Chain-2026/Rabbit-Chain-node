#!/usr/bin/env bash
# Storage maintenance resilience smoke:
# - Generate a real RocksDB compute workload through RPC.
# - Verify rebuild dry-run does not replace the DB.
# - Verify rebuild refuses to run while the DB is locked by a live node.
# - Verify real rebuild keeps a backup.
# - Verify prune dry-run does not delete and real prune does delete old entries.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${ROOT_DIR}/artifacts/storage-resilience-smoke"
RUN_DIR="${REPORT_DIR}/run"
REPORT_FILE="${REPORT_DIR}/report.md"
LOG_DIR="${REPORT_DIR}/logs"
NODE_LOG="${LOG_DIR}/node.log"

RPC_PORT="${RPC_PORT:-28455}"
WS_PORT="${WS_PORT:-28456}"
P2P_PORT="${P2P_PORT:-38455}"
RPC_AUTH_TOKEN="${RPC_AUTH_TOKEN:-storage-resilience-smoke-token}"
RPC_URL="http://127.0.0.1:${RPC_PORT}"
FIXTURE_FILE="${FIXTURE_FILE:-${ROOT_DIR}/fixtures/compute_json/ed25519_owner_mint.json}"

NODE_PID=""

usage() {
  cat <<'EOF'
Usage: bash scripts/storage_resilience_smoke.sh

Environment overrides:
  RPC_PORT        Local rabbitchain RPC port (default: 28455)
  WS_PORT         Local rabbitchain WS port (default: 28456)
  P2P_PORT        Local rabbitchain P2P port (default: 38455)
  RPC_AUTH_TOKEN  Auth token required by node RPC write methods
  FIXTURE_FILE    Compute fixture JSON with top-level {"input":...}
EOF
}

while (($# > 0)); do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

cleanup() {
  if [[ -n "${NODE_PID}" ]]; then
    kill "${NODE_PID}" >/dev/null 2>&1 || true
    wait "${NODE_PID}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing command: $1" >&2
    exit 1
  }
}

assert_port_free() {
  local port="$1"
  if ss -ltn | grep -q ":${port}\\b"; then
    echo "Port ${port} is already in use" >&2
    exit 1
  fi
}

rpc_call() {
  local method="$1"
  local params_json="$2"
  curl -fsS \
    -H 'content-type: application/json' \
    -H "authorization: Bearer ${RPC_AUTH_TOKEN}" \
    --data "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params_json}}" \
    "${RPC_URL}"
}

wait_rpc_ok() {
  local timeout_secs="${1:-60}"
  local i=0
  while (( i < timeout_secs )); do
    if rpc_call net_version '[]' >/dev/null 2>&1; then
      return 0
    fi
    i=$((i + 1))
    sleep 1
  done
  echo "Timeout waiting for RPC ${RPC_URL}" >&2
  return 1
}

extract_compute_input() {
  python3 - "${FIXTURE_FILE}" "${RUN_DIR}/compute-input.json" <<'PY'
import json
import sys

src, dest = sys.argv[1], sys.argv[2]
with open(src, "r", encoding="utf-8") as fh:
    payload = json.load(fh)
with open(dest, "w", encoding="utf-8") as fh:
    json.dump(payload["input"], fh, ensure_ascii=True, indent=2)
    fh.write("\n")
PY
}

submit_compute_tx() {
  "${ROOT_DIR}/target/debug/rabbitchain" \
    --rpc-url "${RPC_URL}" \
    --rpc-token "${RPC_AUTH_TOKEN}" \
    compute send \
    --tx-file "${RUN_DIR}/compute-input.json"
}

require_cmd cargo
require_cmd curl
require_cmd python3
require_cmd ss

assert_port_free "${RPC_PORT}"
assert_port_free "${WS_PORT}"
assert_port_free "${P2P_PORT}"

rm -rf "${RUN_DIR}"
mkdir -p "${RUN_DIR}" "${LOG_DIR}"

echo "==> Build rabbitchain CLI"
cargo build -p rabbitcli >/dev/null

extract_compute_input

echo "==> Start node with RocksDB compute backend"
"${ROOT_DIR}/target/debug/rabbitchain" \
  --network mainnet \
  --data-dir "${RUN_DIR}/node" \
  run \
  --http-port "${RPC_PORT}" \
  --ws-port "${WS_PORT}" \
  --p2p-listen-addr 127.0.0.1 \
  --p2p-listen-port "${P2P_PORT}" \
  --disable-discovery \
  --disable-sync \
  --rpc-auth-token "${RPC_AUTH_TOKEN}" \
  --rpc-rate-limit-per-minute 0 \
  >"${NODE_LOG}" 2>&1 &
NODE_PID="$!"
wait_rpc_ok 60

echo "==> Submit compute tx into RocksDB"
compute_output="$(submit_compute_tx)"
canonical_tx_id="$(printf '%s' "${compute_output}" | sed -n 's/^canonical_tx_id: \(0x[0-9a-fA-F]\+\)$/\1/p')"
if [[ -z "${canonical_tx_id}" ]]; then
  echo "Failed to extract canonical tx id" >&2
  echo "${compute_output}" >&2
  exit 1
fi

COMPUTE_DB="${RUN_DIR}/node/compute-db"

echo "==> Verify rebuild refuses locked DB"
set +e
locked_output="$("${ROOT_DIR}/target/debug/rabbitchain" storage rebuild-compute-db --compute-backend rocksdb --compute-db-path "${COMPUTE_DB}" --dry-run 2>&1)"
locked_status=$?
set -e
if [[ "${locked_status}" -eq 0 ]]; then
  echo "Expected rebuild dry-run to fail while DB is locked by running node" >&2
  echo "${locked_output}" >&2
  exit 1
fi

echo "==> Stop node"
kill "${NODE_PID}" >/dev/null 2>&1 || true
wait "${NODE_PID}" >/dev/null 2>&1 || true
NODE_PID=""

echo "==> Rebuild dry-run on populated DB"
rebuild_dry_output="$("${ROOT_DIR}/target/debug/rabbitchain" storage rebuild-compute-db --compute-backend rocksdb --compute-db-path "${COMPUTE_DB}" --dry-run)"
if ! printf '%s' "${rebuild_dry_output}" | grep -q 'source entries: 3'; then
  echo "Expected rebuild dry-run to scan 3 entries" >&2
  echo "${rebuild_dry_output}" >&2
  exit 1
fi
if find "${RUN_DIR}/node" -maxdepth 1 -name 'compute-db.backup-*' | grep -q .; then
  echo "Dry-run unexpectedly created a backup" >&2
  exit 1
fi

echo "==> Real rebuild on populated DB"
rebuild_output="$("${ROOT_DIR}/target/debug/rabbitchain" storage rebuild-compute-db --compute-backend rocksdb --compute-db-path "${COMPUTE_DB}")"
if ! printf '%s' "${rebuild_output}" | grep -q 'installed entries: 3'; then
  echo "Expected real rebuild to install 3 entries" >&2
  echo "${rebuild_output}" >&2
  exit 1
fi
backup_count="$(find "${RUN_DIR}/node" -maxdepth 1 -name 'compute-db.backup-*' | wc -l | tr -d ' ')"
if (( backup_count < 1 )); then
  echo "Real rebuild did not create a backup" >&2
  exit 1
fi

echo "==> Prune dry-run and real prune"
prune_dry_output="$("${ROOT_DIR}/target/debug/rabbitchain" storage prune-compute-db --compute-backend rocksdb --compute-db-path "${COMPUTE_DB}" --retention-profile mainnet --retention-window-secs 1 --now-unix-secs 9999999999 --dry-run)"
if ! printf '%s' "${prune_dry_output}" | grep -q 'deleted entries: 0'; then
  echo "Prune dry-run deleted entries unexpectedly" >&2
  echo "${prune_dry_output}" >&2
  exit 1
fi
prune_output="$("${ROOT_DIR}/target/debug/rabbitchain" storage prune-compute-db --compute-backend rocksdb --compute-db-path "${COMPUTE_DB}" --retention-profile mainnet --retention-window-secs 1 --now-unix-secs 9999999999)"
if ! printf '%s' "${prune_output}" | grep -q 'deleted entries: 1'; then
  echo "Expected real prune to delete old tx result only" >&2
  echo "${prune_output}" >&2
  exit 1
fi

DATE_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
COMMIT="$(git -C "${ROOT_DIR}" rev-parse --short HEAD)"
cat > "${REPORT_FILE}" <<EOF
# Storage Resilience Smoke Report

- Generated at: ${DATE_UTC}
- Commit: ${COMMIT}
- Compute DB: ${COMPUTE_DB}
- Canonical compute tx: ${canonical_tx_id}

## Checks

- [x] Populated RocksDB compute DB through RPC compute submission.
- [x] Rebuild dry-run fails while RocksDB is locked by a live node.
- [x] Rebuild dry-run scans populated DB and creates no backup.
- [x] Real rebuild installs rebuilt DB and keeps backup.
- [x] Prune dry-run deletes nothing.
- [x] Real prune deletes old tx result while leaving live output data.

## Locked Rebuild Output

\`\`\`text
${locked_output}
\`\`\`

## Rebuild Dry-Run Output

\`\`\`text
${rebuild_dry_output}
\`\`\`

## Real Rebuild Output

\`\`\`text
${rebuild_output}
\`\`\`

## Prune Output

\`\`\`text
${prune_output}
\`\`\`
EOF

echo "✅ storage resilience smoke passed"
echo "📄 Report: ${REPORT_FILE}"
