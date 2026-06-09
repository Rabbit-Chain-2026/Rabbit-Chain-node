#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  scripts/perf_compute_tps.sh [local|submit-benchmark] [options]

Modes:
  local             Run the standard local compute TPS benchmark.
  submit-benchmark  Run the submit benchmark against a live RPC endpoint.

Options:
  --tx-count N
  --ingress-concurrency N
  --direct-flush-every N
  --persist-batch-size N
  --rpc-urls URL1,URL2,...
  --rpc-url URL
  --rpc-token TOKEN
  --artifact-root PATH
  -h, --help
EOF
}

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

REQUESTED_MODE="local"
if [[ $# -gt 0 && "${1:-}" != --* ]]; then
  REQUESTED_MODE="$1"
  shift
fi

MODE="$REQUESTED_MODE"
case "$MODE" in
  local|submit-benchmark|real-rpc) ;;
  -h|--help|help)
    usage
    exit 0
    ;;
  *)
    echo "Unknown mode: $MODE" >&2
    usage
    exit 1
    ;;
esac

if [[ "$MODE" == "real-rpc" ]]; then
  MODE="submit-benchmark"
fi

TX_COUNT="${RABBIT_TPS_TX_COUNT:-}"
INGRESS_CONCURRENCY="${RABBIT_TPS_INGRESS_CONCURRENCY:-}"
DIRECT_FLUSH_EVERY="${RABBIT_TPS_DIRECT_FLUSH_EVERY:-}"
PERSIST_BATCH_SIZE="${RABBIT_TPS_PERSIST_BATCH_SIZE:-}"
RPC_URLS="${RABBIT_TPS_RPC_URLS:-}"
RPC_URL="${RABBIT_TPS_RPC_URL:-}"
RPC_TOKEN="${RABBIT_TPS_RPC_TOKEN:-}"
ARTIFACT_ROOT="${RABBIT_TPS_ARTIFACT_ROOT:-artifacts/perf/compute-tps}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tx-count)
      TX_COUNT="${2:?missing value for --tx-count}"
      shift 2
      ;;
    --ingress-concurrency)
      INGRESS_CONCURRENCY="${2:?missing value for --ingress-concurrency}"
      shift 2
      ;;
    --direct-flush-every)
      DIRECT_FLUSH_EVERY="${2:?missing value for --direct-flush-every}"
      shift 2
      ;;
    --persist-batch-size)
      PERSIST_BATCH_SIZE="${2:?missing value for --persist-batch-size}"
      shift 2
      ;;
    --rpc-urls)
      RPC_URLS="${2:?missing value for --rpc-urls}"
      shift 2
      ;;
    --rpc-url)
      RPC_URL="${2:?missing value for --rpc-url}"
      shift 2
      ;;
    --rpc-token)
      RPC_TOKEN="${2:?missing value for --rpc-token}"
      shift 2
      ;;
    --artifact-root)
      ARTIFACT_ROOT="${2:?missing value for --artifact-root}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage
      exit 1
      ;;
  esac
done

case "$MODE" in
  local)
    TX_COUNT="${TX_COUNT:-1000000}"
    INGRESS_CONCURRENCY="${INGRESS_CONCURRENCY:-128}"
    DIRECT_FLUSH_EVERY="${DIRECT_FLUSH_EVERY:-50000}"
    PERSIST_BATCH_SIZE="${PERSIST_BATCH_SIZE:-128}"
    TEST_NAME="compute_tps_benchmark"
    ;;
  submit-benchmark)
    TX_COUNT="${TX_COUNT:-1000000}"
    INGRESS_CONCURRENCY="${INGRESS_CONCURRENCY:-256}"
    DIRECT_FLUSH_EVERY="${DIRECT_FLUSH_EVERY:-50000}"
    PERSIST_BATCH_SIZE="${PERSIST_BATCH_SIZE:-128}"
    TEST_NAME="compute_tps_submit_benchmark"
    if [[ -z "$RPC_URLS" && -z "$RPC_URL" ]]; then
      echo "submit-benchmark mode requires RABBIT_TPS_RPC_URLS or RABBIT_TPS_RPC_URL, plus RABBIT_TPS_RPC_TOKEN" >&2
      exit 1
    fi
    if [[ -z "$RPC_TOKEN" ]]; then
      echo "submit-benchmark mode requires RABBIT_TPS_RPC_TOKEN, or --rpc-token" >&2
      exit 1
    fi
    ;;
esac

timestamp="$(date -u +%Y%m%dT%H%M%SZ)"
git_rev="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
git_branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
run_dir="$ARTIFACT_ROOT/$MODE/$timestamp"
mkdir -p "$run_dir"

log_file="$run_dir/run.log"
meta_file="$run_dir/meta.txt"
report_file="$run_dir/report.md"
start_epoch="$(date +%s)"
start_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
cargo_profile="release"
if [[ "$MODE" == "local" ]]; then
  command_line="RABBIT_TPS_TX_COUNT=$TX_COUNT RABBIT_TPS_INGRESS_CONCURRENCY=$INGRESS_CONCURRENCY RABBIT_TPS_DIRECT_FLUSH_EVERY=$DIRECT_FLUSH_EVERY RABBIT_TPS_PERSIST_BATCH_SIZE=$PERSIST_BATCH_SIZE cargo test --release -p rabbitapi --test compute_tps_bench $TEST_NAME -- --ignored --nocapture"
else
  if [[ -n "$RPC_URLS" ]]; then
    command_line="RABBIT_TPS_RPC_URLS=$RPC_URLS RABBIT_TPS_RPC_TOKEN=<redacted> RABBIT_TPS_TX_COUNT=$TX_COUNT RABBIT_TPS_INGRESS_CONCURRENCY=$INGRESS_CONCURRENCY cargo test --release -p rabbitapi --test compute_tps_bench $TEST_NAME -- --ignored --nocapture"
  else
    command_line="RABBIT_TPS_RPC_URL=<set> RABBIT_TPS_RPC_TOKEN=<redacted> RABBIT_TPS_TX_COUNT=$TX_COUNT RABBIT_TPS_INGRESS_CONCURRENCY=$INGRESS_CONCURRENCY cargo test --release -p rabbitapi --test compute_tps_bench $TEST_NAME -- --ignored --nocapture"
  fi
fi

cat >"$meta_file" <<EOF
mode=$MODE
requested_mode=$REQUESTED_MODE
git_rev=$git_rev
git_branch=$git_branch
tx_count=$TX_COUNT
ingress_concurrency=$INGRESS_CONCURRENCY
direct_flush_every=$DIRECT_FLUSH_EVERY
persist_batch_size=$PERSIST_BATCH_SIZE
rpc_url=${RPC_URL:-<not-set>}
rpc_urls=${RPC_URLS:-<not-set>}
rpc_mode=$MODE
cargo_profile=$cargo_profile
EOF

write_report() {
  local exit_code="$1"
  local status="success"
  if [[ "$exit_code" -ne 0 ]]; then
    status="failure"
  fi

  local end_epoch
  local end_utc
  end_epoch="$(date +%s)"
  end_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  local duration_seconds=$((end_epoch - start_epoch))

  {
    printf '# Compute TPS Run Report\n\n'
    printf -- '- mode: %s\n' "$MODE"
    printf -- '- requested_mode: %s\n' "$REQUESTED_MODE"
    printf -- '- status: %s\n' "$status"
    printf -- '- exit_code: %s\n' "$exit_code"
    printf -- '- start_utc: %s\n' "$start_utc"
    printf -- '- end_utc: %s\n' "$end_utc"
    printf -- '- duration_seconds: %s\n' "$duration_seconds"
    printf -- '- git_rev: %s\n' "$git_rev"
    printf -- '- git_branch: %s\n' "$git_branch"
    printf -- '- tx_count: %s\n' "$TX_COUNT"
    printf -- '- ingress_concurrency: %s\n' "$INGRESS_CONCURRENCY"
    printf -- '- direct_flush_every: %s\n' "$DIRECT_FLUSH_EVERY"
    printf -- '- persist_batch_size: %s\n' "$PERSIST_BATCH_SIZE"
    printf -- '- rpc_url: %s\n' "${RPC_URL:-<not-set>}"
    printf -- '- rpc_urls: %s\n' "${RPC_URLS:-<not-set>}"
    printf -- '- rpc_mode: %s\n' "$MODE"
    printf -- '- cargo_profile: %s\n' "$cargo_profile"
    printf -- '- log_file: run.log\n'
    printf -- '- meta_file: meta.txt\n\n'
    printf '## Command\n\n'
    printf '```bash\n%s\n```\n' "$command_line"
  } >"$report_file"

  {
    echo "mode=$MODE"
    echo "status=$status"
    echo "exit_code=$exit_code"
    echo "start_utc=$start_utc"
    echo "end_utc=$end_utc"
    echo "duration_seconds=$duration_seconds"
    echo "report_file=$report_file"
  } >>"$meta_file"
}

trap 'write_report "$?"' EXIT

echo "[perf] run_dir: $run_dir"
echo "[perf] log_file: $log_file"
echo "[perf] meta_file: $meta_file"
echo "[perf] report_file: $report_file"

case "$MODE" in
  local)
    env \
      RABBIT_TPS_TX_COUNT="$TX_COUNT" \
      RABBIT_TPS_INGRESS_CONCURRENCY="$INGRESS_CONCURRENCY" \
      RABBIT_TPS_DIRECT_FLUSH_EVERY="$DIRECT_FLUSH_EVERY" \
      RABBIT_TPS_PERSIST_BATCH_SIZE="$PERSIST_BATCH_SIZE" \
      cargo test --release -p rabbitapi --test compute_tps_bench "$TEST_NAME" -- --ignored --nocapture \
      2>&1 \
      | tee "$log_file"
    ;;
  submit-benchmark)
    if [[ -n "$RPC_URLS" ]]; then
      env \
        RABBIT_TPS_RPC_URLS="$RPC_URLS" \
        RABBIT_TPS_RPC_TOKEN="$RPC_TOKEN" \
        RABBIT_TPS_TX_COUNT="$TX_COUNT" \
        RABBIT_TPS_INGRESS_CONCURRENCY="$INGRESS_CONCURRENCY" \
        cargo test --release -p rabbitapi --test compute_tps_bench "$TEST_NAME" -- --ignored --nocapture \
        2>&1 \
        | tee "$log_file"
    else
      env \
        RABBIT_TPS_RPC_URL="$RPC_URL" \
        RABBIT_TPS_RPC_TOKEN="$RPC_TOKEN" \
        RABBIT_TPS_TX_COUNT="$TX_COUNT" \
        RABBIT_TPS_INGRESS_CONCURRENCY="$INGRESS_CONCURRENCY" \
        cargo test --release -p rabbitapi --test compute_tps_bench "$TEST_NAME" -- --ignored --nocapture \
        2>&1 \
        | tee "$log_file"
    fi
    ;;
esac

echo "[perf] completed successfully"
