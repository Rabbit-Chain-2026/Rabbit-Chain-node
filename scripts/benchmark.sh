#!/usr/bin/env bash

set -euo pipefail

echo "[deprecated] scripts/benchmark.sh is deprecated; use scripts/perf_compute_tps.sh instead." >&2
exec "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/perf_compute_tps.sh" "$@"
