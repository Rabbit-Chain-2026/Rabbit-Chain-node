#!/usr/bin/env bash
# RabbitChain release gate runner

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
REPORT_DIR="${ROOT_DIR}/artifacts/release-gate"
REPORT_FILE="${REPORT_DIR}/go-no-go-report.md"

mkdir -p "${REPORT_DIR}"

echo "🧪 RabbitChain release gate"
echo "========================="

echo "[1/5] redline guard (no silent fallback)"
bash scripts/no_silent_fallback.sh

echo "[2/5] cargo fmt --all --check"
cargo fmt --all --check

echo "[3/5] cargo check --workspace"
cargo check --workspace

echo "[4/5] cargo test --workspace"
cargo test --workspace

echo "[5/5] cargo test --workspace -- --ignored"
set +e
cargo test --workspace -- --ignored
IGNORED_EXIT=$?
set -e

if [[ ${IGNORED_EXIT} -eq 0 ]]; then
  IGNORED_STATUS="PASS"
  IGNORED_CHECK="x"
else
  IGNORED_STATUS="FAIL (non-blocking informational)"
  IGNORED_CHECK=" "
fi

COMMIT="$(git -C "${ROOT_DIR}" rev-parse --short HEAD)"
TAG="$(git -C "${ROOT_DIR}" tag --points-at HEAD | tr '\n' ' ' || true)"
DATE_UTC="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

cat > "${REPORT_FILE}" <<EOF
# RabbitChain Go/No-Go Report

- Generated at: ${DATE_UTC}
- Commit: ${COMMIT}
- Tag(s): ${TAG}

## Automated Gates

- [x] redline guard (no silent fallback)
- [x] cargo fmt --all --check
- [x] cargo check --workspace
- [x] cargo test --workspace
- [${IGNORED_CHECK}] cargo test --workspace -- --ignored (status: ${IGNORED_STATUS})

## Manual Blocking Items (from docs/GO_NO_GO_CHECKLIST.md)

- [ ] Security audit (E1)
- [ ] Secrets management / key rotation validation (E3)
- [ ] Observability and alerts drill (F1-F4)
- [ ] Performance/load + soak tests (G1-G3)
- [ ] Rollback rehearsal completion

## Preliminary Decision

- Automated code gates: PASS
- Ignored-tests informational status: ${IGNORED_STATUS}
- Production release decision: NO-GO until manual blocking items close
EOF

echo "✅ Release gate completed"
echo "📄 Report: ${REPORT_FILE}"
