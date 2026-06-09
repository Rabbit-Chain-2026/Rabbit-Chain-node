#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
WORKSPACE_DIR="${WORKSPACE_DIR:-${ROOT_DIR}/..}"
WALLET_CHROME_DIR="${WALLET_CHROME_DIR:-${WORKSPACE_DIR}/rabbitchain-wallet-chrome}"
WALLET_MOBILE_DIR="${WALLET_MOBILE_DIR:-${WORKSPACE_DIR}/rabbitchain-wallet-mobile}"

echo "==> rabbitapi compute json fixtures"
bash -lc "cd '${ROOT_DIR}' && cargo test -p rabbitapi compute_json_fixture_ -- --nocapture"

echo "==> rabbitchain-wallet-chrome compute json fixtures"
bash -lc "cd '${WALLET_CHROME_DIR}' && bun test src/core/wallet/ComputeTx.fixture.test.ts"

echo "==> rabbitchain-wallet-mobile compute json fixtures"
bash -lc "cd '${WALLET_MOBILE_DIR}' && flutter test test/core/utils/compute_tx_fixture_test.dart"

echo "✅ compute json fixtures passed"
