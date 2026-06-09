# RabbitChain

A blockchain with a body-first canonical block model, native UTXO Compute execution, and PoW security.

## Features

- Body-first canonical block model and native UTXO Compute execution
- ed25519 account and signing flow
- PoW consensus and P2P networking
- JSON-RPC + WebSocket service surface
- CLI for node, wallet, account, compute, block, body, and receipt operations

## Block Model

- Canonical blocks are body-first: a block carries `header`, optional `body`, and `uncles`.
- `transactions_root` and `receipts_root` commit to the body transactions and execution receipts for newly produced blocks.
- Legacy blocks remain readable for compatibility and historical queries.

## Quick Start

```bash
# Clone
git clone https://github.com/Rabbit-Chain-2026/Rabbit-Chain-node.git
cd Rabbit-Chain-node

# Build
cargo build --release

# Run tests
cargo test
```

## Run a Node

```bash
# Initialize data directory (once per network profile)
./target/release/rabbitchain --network local init
./target/release/rabbitchain --network testnet init
./target/release/rabbitchain --network devnet init
./target/release/rabbitchain --network mainnet init

# Run local profile
./target/release/rabbitchain --network local run

# Run testnet profile
./target/release/rabbitchain --network testnet run

# Run devnet profile
./target/release/rabbitchain --network devnet run

# Run mainnet profile
./target/release/rabbitchain --network mainnet run
```

## CLI Examples

```bash
# Create native wallet account
rabbitchain wallet new --name ed25519-1 --scheme ed25519

# List wallet accounts
rabbitchain wallet list

# Account alias command (delegates to wallet)
rabbitchain account new --name ed25519-2 --scheme ed25519
rabbitchain account list

# Sign message (prompts for passphrase if the wallet is not unlocked)
rabbitchain wallet sign --name ed25519-1 --message "hello"

# Unlock then sign without passphrase
rabbitchain wallet unlock --name ed25519-1 --ttl-secs 600
rabbitchain wallet sign --name ed25519-1 --message "hello"

# Submit compute operation from JSON file
rabbitchain --rpc-token YOUR_RPC_TOKEN compute send --tx-file ./tx.json

# Query compute operation result
rabbitchain --rpc-token YOUR_RPC_TOKEN compute get --tx-id 0x...
```

完整的钱包建立、coinbase 配置、内置挖矿、外部 pool/miner 挖矿教程见：

- [快速入门](docs/book/10-foundations/getting-started.md)
- [挖矿流程](docs/book/60-mining/mining-flow.md)
- [旧文档索引](docs/book/95-legacy/archive.md)

Compute JSON 共享规范见：
- [Compute JSON 规范](docs/book/20-transaction-path/compute-json.md)

## RPC Example

```bash
# Default RPC ports:
# - local/mainnet: 8545
# - testnet: 18545
# - devnet: 28545
curl -X POST http://localhost:8545 \
  -H "Authorization: Bearer YOUR_RPC_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getAccount","params":["0x..."],"id":1}'
```

说明：
- 未配置 `auth_token` 时，读方法默认可访问，但有状态写方法会直接拒绝。
- 配置 `auth_token` 后，所有 RPC 请求都需要携带 token。

## Development

```bash
# Redline guard (禁止 silent fallback)
bash scripts/no_silent_fallback.sh

# 指定目录检查（可重复 -d）
bash scripts/no_silent_fallback.sh -d ../Rabbit-Chain-node -d ../rabbitchain-explorer

# Format
cargo fmt

# Lint
cargo clippy -- -D warnings

# Tests
cargo test
```

## Performance Benchmarking

Compute TPS 的公开测试教程和报告见：

- [性能基准](docs/book/70-operations/benchmarks.md)

Compute TPS 的标准操作手册见：

- [运行手册](docs/book/70-operations/runbook.md)
- `scripts/perf_compute_tps.sh`

这套测试口径分两种：

- 本地性能基准：默认 `1,000,000` 交易，自动生成 `run.log`、`meta.txt`、`report.md`
- 提交基准：通过同一套脚本对接本地或远端 RPC 端点，保持相同的留档格式
- 真实挖矿 e2e：通过 `scripts/mining_e2e.sh` 记录 compute 执行结果、链上 receipt 观测、区块高度和矿池/矿工 share

本地标准档：

```bash
bash scripts/perf_compute_tps.sh
```

提交基准：

```bash
RABBIT_TPS_RPC_URL=https://example-rpc \
RABBIT_TPS_RPC_TOKEN=your_token \
bash scripts/perf_compute_tps.sh submit-benchmark
```

真实挖矿 e2e：

```bash
bash scripts/mining_e2e.sh
```

如果你要做真实交易是否被执行、是否出块、是否有 share 的完整闭环，优先用这个入口，而不是 `submit-benchmark`。

## Engineering Redlines

- 设计原则：`docs/book/00-preface/design-principles.md`
- 工程红线：`docs/book/00-preface/engineering-redlines.md`
- CI 阻断：`.github/workflows/redline-guard.yml`
- 发布门禁包含 redline 检查：`scripts/run_tests.sh`

## Release Gates

- 发布就绪：`docs/book/70-operations/release-readiness.md`
- 密钥托管与轮换验收：`docs/KEY_MANAGEMENT_ACCEPTANCE.md`

## Mainnet Checklist

```bash
# Public local + remote + observer + explorer checklist
./scripts/mainnet_checklist.sh
```

## Mainnet Bring-up

受控启网与受控真实挖矿 runbook：

- [主网拉起](docs/book/70-operations/mainnet-bringup.md)
- [旧文档索引](docs/book/95-legacy/archive.md)

主节点启动入口：

```bash
./scripts/mainnet.sh start bootnode --mine --coinbase 0xYOUR_COINBASE
```

## Workspace Acceptance

统一验收当前多仓工作区：

```bash
cd Rabbit-Chain-node
bash scripts/workspace_acceptance.sh --quick
```

完整模式：

```bash
cd Rabbit-Chain-node
bash scripts/workspace_acceptance.sh --full
```

详细口径见：

- [Workspace 验收](docs/book/70-operations/workspace-acceptance.md)

本地 CLI + 外部矿工最小闭环 smoke：

```bash
cd Rabbit-Chain-node
bash scripts/cli_mining_smoke.sh
```

本地主网严格口径 smoke（mainnet 拓扑 + RocksDb + 默认限流 + RPC 鉴权）：

```bash
cd Rabbit-Chain-node
bash scripts/mainnet_strict_smoke.sh
```

Key checks include:
- local/remote/observer RPC reachability, peerCount, block heights, `rabbit_syncStatus`
- local/remote block-gap threshold
- explorer `/health`, `/api/overview`, `/api/txs/recent`, account balance + account tx endpoints
- public soak monitor health and RPC/SSH error counters

## License

MIT OR Apache-2.0
