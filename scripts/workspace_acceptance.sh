#!/usr/bin/env bash
# Unified workspace acceptance for Rabbit-Chain-node + rabbitchain-explorer + rabbitchain-mining-stack + wallets.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORKSPACE_DIR="${WORKSPACE_DIR:-${ROOT_DIR}/..}"
MINING_STACK_DIR="${MINING_STACK_DIR:-${WORKSPACE_DIR}/rabbitchain-mining-stack}"
EXPLORER_DIR="${EXPLORER_DIR:-${WORKSPACE_DIR}/rabbitchain-explorer}"
EXPLORER_BACKEND_DIR="${EXPLORER_BACKEND_DIR:-${EXPLORER_DIR}/backend}"
WALLET_CHROME_DIR="${WALLET_CHROME_DIR:-${WORKSPACE_DIR}/rabbitchain-wallet-chrome}"
WALLET_MOBILE_DIR="${WALLET_MOBILE_DIR:-${WORKSPACE_DIR}/rabbitchain-wallet-mobile}"

REPORT_DIR="${ROOT_DIR}/artifacts/workspace-acceptance"
REPORT_FILE="${REPORT_DIR}/workspace-acceptance-report.md"
mkdir -p "${REPORT_DIR}"

DATE_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
MODE="full"

usage() {
  cat <<'EOF'
Usage: bash scripts/workspace_acceptance.sh [--quick|--full]

  --quick  Run the fast local acceptance subset
  --full   Run the full cross-repo acceptance suite (default)
EOF
}

while (($# > 0)); do
  case "$1" in
    --quick)
      MODE="quick"
      shift
      ;;
    --full)
      MODE="full"
      shift
      ;;
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

run_step() {
  local name="$1"
  shift
  echo "==> ${name}"
  "$@"
}

assert_dir() {
  local dir="$1"
  if [[ ! -d "${dir}" ]]; then
    echo "Missing directory: ${dir}" >&2
    exit 1
  fi
}

assert_dir "${MINING_STACK_DIR}"
assert_dir "${EXPLORER_BACKEND_DIR}"
assert_dir "${WALLET_CHROME_DIR}"
assert_dir "${WALLET_MOBILE_DIR}"

AUTOMATED_CHECKS=()

record_check() {
  AUTOMATED_CHECKS+=("$1")
}

run_step "rabbitchain-wallet-chrome build" bash -lc "cd '${WALLET_CHROME_DIR}' && bun run build"
record_check "rabbitchain-wallet-chrome bun run build"
run_step "rabbitchain-wallet-chrome tests" bash -lc "cd '${WALLET_CHROME_DIR}' && bun run test"
record_check "rabbitchain-wallet-chrome bun run test"
run_step "cross-stack compute json fixtures" bash -lc "cd '${ROOT_DIR}' && bash scripts/check_compute_json_fixtures.sh"
  record_check "Rabbit-Chain-node/scripts/check_compute_json_fixtures.sh"

run_step "rabbitchain-wallet-mobile analyze" bash -lc "cd '${WALLET_MOBILE_DIR}' && flutter analyze"
record_check "rabbitchain-wallet-mobile flutter analyze"
run_step "rabbitchain-wallet-mobile tests" bash -lc "cd '${WALLET_MOBILE_DIR}' && flutter test"
record_check "rabbitchain-wallet-mobile flutter test"

if [[ "${MODE}" == "quick" ]]; then
  run_step "Rabbit-Chain-node full-chain e2e" bash -lc "cd '${ROOT_DIR}' && bash scripts/full_chain_e2e.sh"
  record_check "Rabbit-Chain-node/scripts/full_chain_e2e.sh"
else
  run_step "Rabbit-Chain-node full-chain e2e" bash -lc "cd '${ROOT_DIR}' && bash scripts/full_chain_e2e.sh"
  record_check "Rabbit-Chain-node/scripts/full_chain_e2e.sh"
  run_step "rabbitchain-mining-stack nightly local qa" bash -lc "cd '${MINING_STACK_DIR}' && bash scripts/nightly_local_qa.sh"
  record_check "rabbitchain-mining-stack/scripts/nightly_local_qa.sh"
  run_step "rabbitchain-wallet-chrome extension smoke" bash -lc "cd '${WALLET_CHROME_DIR}' && bun run qa:extension"
  record_check "rabbitchain-wallet-chrome bun run qa:extension"
  run_step "rabbitchain-wallet-mobile devices" bash -lc "cd '${WALLET_MOBILE_DIR}' && flutter devices"
  record_check "rabbitchain-wallet-mobile flutter devices"
fi

CHECKS_MARKDOWN=""
for check in "${AUTOMATED_CHECKS[@]}"; do
  CHECKS_MARKDOWN+="- [x] ${check}"$'\n'
done

cat > "${REPORT_FILE}" <<EOF
# Workspace Acceptance Report

- Generated at: ${DATE_UTC}
- Workspace: ${WORKSPACE_DIR}
- Mode: ${MODE}

## Automated Checks

${CHECKS_MARKDOWN}

## Manual Follow-up

- [ ] If a GUI session is available, run rabbitchain-wallet-mobile with \`flutter run -d linux\` or a real Android device
- [ ] If browser sandbox policy allows, run rabbitchain-wallet-mobile with \`flutter run -d chrome\`
- [ ] If multi-node mirroring is required, rerun rabbitchain-mining-stack pool with explicit \`--mirror-peer\` values
EOF

echo "✅ workspace acceptance passed"
echo "📄 Report: ${REPORT_FILE}"
