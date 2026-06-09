---
id: legacy.archive
title: 旧文档索引
kind: legacy
status: verified
owner: core-docs
primary_topic: legacy.archive
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: scripts/docs-check.py
  - type: code
    ref: scripts/docs-build-nav.py
  - type: doc
    ref: docs/manifest.yaml
  - type: doc
    ref: docs/coverage.yaml
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 旧文档索引

这页不是正文，只是旧文档的去向表。

## 已并入新书库的内容

| 旧文档 | 当前去向 |
|---|---|
| [`docs/API.md`](../../API.md) | [API 参考](../10-foundations/api.md) |
| [`docs/COMPUTE_JSON_SPEC.md`](../../COMPUTE_JSON_SPEC.md) | [Compute JSON 规范](../20-transaction-path/compute-json.md) |
| [`docs/COMPUTE_TPS_BENCHMARK_RUNBOOK.md`](../../COMPUTE_TPS_BENCHMARK_RUNBOOK.md) | [运行手册](../70-operations/runbook.md) / [性能基准](../70-operations/benchmarks.md) |
| [`docs/COMPUTE_TPS_BENCHMARK_TUTORIAL.md`](../../COMPUTE_TPS_BENCHMARK_TUTORIAL.md) | [运行手册](../70-operations/runbook.md) |
| [`docs/COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md`](../../COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md) | [性能基准](../70-operations/benchmarks.md) |
| [`docs/CLI_WALLET_MINING_TUTORIAL.md`](../../CLI_WALLET_MINING_TUTORIAL.md) | [运行手册](../70-operations/runbook.md) / [RPC 认证](../80-security/auth.md) / [挖矿](../60-mining/mining-flow.md) |
| [`docs/FULL_CHAIN_E2E_2026-03-07.md`](../../FULL_CHAIN_E2E_2026-03-07.md) | [运行手册](../70-operations/runbook.md) / [状态快照](../50-sync/snapshot.md) / [重组](../50-sync/reorg.md) |
| [`docs/GETTING_STARTED.md`](../../GETTING_STARTED.md) | [快速入门](../10-foundations/getting-started.md) |
| [`docs/DESIGN_PHILOSOPHY.md`](../../DESIGN_PHILOSOPHY.md) | [设计原则](../00-preface/design-principles.md) |
| [`docs/ENGINEERING_REDLINES.md`](../../ENGINEERING_REDLINES.md) | [工程红线](../00-preface/engineering-redlines.md) |
| [`docs/MAINNET_BRINGUP_RUNBOOK.md`](../../MAINNET_BRINGUP_RUNBOOK.md) | [主网拉起](../70-operations/mainnet-bringup.md) |
| [`docs/MAINNET_FIRST_WAVE_COMMANDS.md`](../../MAINNET_FIRST_WAVE_COMMANDS.md) | [主网拉起](../70-operations/mainnet-bringup.md) |
| [`docs/MAINNET_LOCAL_BRINGUP.md`](../../MAINNET_LOCAL_BRINGUP.md) | [主网拉起](../70-operations/mainnet-bringup.md) |
| [`docs/MAINNET_NODE_MATRIX.md`](../../MAINNET_NODE_MATRIX.md) | [主网拉起](../70-operations/mainnet-bringup.md) |
| [`docs/MAINNET_REMOTE_BRINGUP.md`](../../MAINNET_REMOTE_BRINGUP.md) | [主网拉起](../70-operations/mainnet-bringup.md) |
| [`docs/MINING.md`](../../MINING.md) | [运行手册](../70-operations/runbook.md) / [体同步](../50-sync/body.md) |
| [`docs/NODE_SYNC_CHECKLIST.md`](../../NODE_SYNC_CHECKLIST.md) | [头同步](../50-sync/headers.md) / [体同步](../50-sync/body.md) / [状态快照](../50-sync/snapshot.md) |
| [`docs/OBSERVABILITY.md`](../../OBSERVABILITY.md) | [运行手册](../70-operations/runbook.md) / [故障排查](../70-operations/troubleshooting.md) |
| [`docs/GO_NO_GO_CHECKLIST.md`](../../GO_NO_GO_CHECKLIST.md) | [发布就绪](../70-operations/release-readiness.md) |
| [`docs/P0_RELEASE_BLOCKERS_2026-03.md`](../../P0_RELEASE_BLOCKERS_2026-03.md) | [发布就绪](../70-operations/release-readiness.md) |
| [`docs/CHAIN_MATURITY_GAP_CHECKLIST.md`](../../CHAIN_MATURITY_GAP_CHECKLIST.md) | [发布就绪](../70-operations/release-readiness.md) |
| [`docs/GO_NO_GO_STATUS_2026-03-27.md`](../../GO_NO_GO_STATUS_2026-03-27.md) | [发布就绪](../70-operations/release-readiness.md) |
| [`docs/P0_P1_P2_EXECUTION_STATUS_2026-03-07.md`](../../P0_P1_P2_EXECUTION_STATUS_2026-03-07.md) | [发布就绪](../70-operations/release-readiness.md) |
| [`docs/CROSS_STACK_COMPLETENESS_2026-03-08.md`](../../CROSS_STACK_COMPLETENESS_2026-03-08.md) | [发布就绪](../70-operations/release-readiness.md) |
| [`docs/POW_TARGET_SYNC_DEBUG_2026-05-31.md`](../../POW_TARGET_SYNC_DEBUG_2026-05-31.md) | [头同步](../50-sync/headers.md) / [重组](../50-sync/reorg.md) |
| [`docs/STORAGE_ARCHITECTURE.md`](../../STORAGE_ARCHITECTURE.md) | [区块存储](../40-storage/block-store.md) / [区块体存储](../40-storage/body-store.md) / [回执索引](../40-storage/receipt-index.md) |
| [`docs/STORAGE_SAVINGS_REPORT.md`](../../STORAGE_SAVINGS_REPORT.md) | [区块存储](../40-storage/block-store.md) |
| [`docs/WORKSPACE_ACCEPTANCE_CHECKLIST.md`](../../WORKSPACE_ACCEPTANCE_CHECKLIST.md) | [Workspace 验收](../70-operations/workspace-acceptance.md) |
| [`docs/RFC_BLOCK_BODY_MIGRATION.md`](../../RFC_BLOCK_BODY_MIGRATION.md) | [区块头](../30-block-model/header.md) / [区块体](../30-block-model/body.md) / [承诺关系](../30-block-model/commitments.md) |
| [`docs/REAL_SYNC_REMEDIATION_2026-03-08.md`](../../REAL_SYNC_REMEDIATION_2026-03-08.md) | [头同步](../50-sync/headers.md) / [体同步](../50-sync/body.md) / [状态快照](../50-sync/snapshot.md) / [重组](../50-sync/reorg.md) |
| [`docs/KEY_MANAGEMENT_ACCEPTANCE.md`](../../KEY_MANAGEMENT_ACCEPTANCE.md) | [密钥](../80-security/keys.md) |
| [`docs/CHAIN_WALLET_SECURITY_AUDIT_2026-05-28.md`](../../CHAIN_WALLET_SECURITY_AUDIT_2026-05-28.md) | [审计](../80-security/audits.md) |
| [`docs/AUDIT_2026-03-16.md`](../../AUDIT_2026-03-16.md) | [审计](../80-security/audits.md) |
| [`docs/P2P_WEBSOCKET_CDN.md`](../../P2P_WEBSOCKET_CDN.md) | [P2P 传输](../40-network/p2p-transport.md) |
| [`docs/UTXO-2.0-YELLOWPAPER.md`](../../UTXO-2.0-YELLOWPAPER.md) | [UTXO Compute 协议](../20-transaction-path/utxo-compute-protocol.md) |
| [`docs/UTXO-Compute-Yellowpaper-v1.1.md`](../../UTXO-Compute-Yellowpaper-v1.1.md) | [UTXO Compute 协议](../20-transaction-path/utxo-compute-protocol.md) |

## 仍保留为 legacy 参考

| 旧文档 | 备注 |
|---|---|

## 归档原则

- 已迁入新书库的内容，以 book 章节为准。
- legacy 文档保留原路径，避免历史引用失效。
- 后续如果某个 legacy 文档被吸收进书库，会先更新本页，再补对应章节。
