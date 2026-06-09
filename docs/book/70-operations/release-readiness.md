---
id: ops.release_readiness
title: 发布就绪
kind: chapter
status: verified
owner: core-docs
primary_topic: ops.release_readiness
topics: []
depends_on:
  - ops.runbook
  - ops.mainnet_bringup
  - ops.benchmarks
aliases:
  - ops.go_no_go
  - ops.release_gate
evidence:
  - type: code
    ref: scripts/run_tests.sh
  - type: code
    ref: scripts/mainnet_strict_smoke.sh
  - type: code
    ref: scripts/mainnet_local_check.sh
  - type: code
    ref: scripts/perf_compute_tps.sh
  - type: code
    ref: scripts/mining_e2e.sh
  - type: code
    ref: scripts/p2p_three_node_smoke.sh
  - type: doc
    ref: docs/GO_NO_GO_CHECKLIST.md
  - type: doc
    ref: docs/P0_RELEASE_BLOCKERS_2026-03.md
  - type: doc
    ref: docs/CHAIN_MATURITY_GAP_CHECKLIST.md
  - type: doc
    ref: docs/GO_NO_GO_STATUS_2026-03-27.md
  - type: doc
    ref: docs/P0_P1_P2_EXECUTION_STATUS_2026-03-07.md
  - type: doc
    ref: docs/CROSS_STACK_COMPLETENESS_2026-03-08.md
  - type: report
    ref: artifacts/mining-e2e/20260604T001819Z/report.md
  - type: report
    ref: docs/COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 发布就绪

这章是发布门禁的汇总层。
它不替代运行手册，也不替代主网拉起步骤，而是回答一个更窄的问题：现在到底能不能放行。

## 这章管什么

- 运行手册管“怎么跑”。
- 主网拉起管“怎么启”。
- 发布就绪管“哪些条件必须同时成立，才算可以放行”。

## 门禁层级

### 自动化门禁

自动化门禁是最基础的一层：

- `scripts/run_tests.sh` 提供统一 release gate。
- `scripts/perf_compute_tps.sh` 提供标准性能/提交基准。
- `scripts/mining_e2e.sh` 提供真实挖矿闭环留档。
- `scripts/mainnet_strict_smoke.sh` 和 `scripts/mainnet_local_check.sh` 提供主网可运行性检查。
- `scripts/p2p_three_node_smoke.sh` 提供多节点同步和连通性检查。

### 手工阻断项

手工项不是“可有可无的加分项”，而是上线前的阻断门槛。
当前文档体系里反复出现的阻断项包括：

- 安全审计
- 密钥与权限管理
- 可观测性与告警
- 性能与长稳
- 回滚演练

### 状态记录

状态记录不是新的协议定义，而是阶段性的事实快照。
它们的作用是把“某时某刻为什么判断为 GO 或 NO-GO”留成证据链。

这也是为什么下面这些文档最终会被吸收到同一章里：

- `GO_NO_GO_CHECKLIST`
- `P0_RELEASE_BLOCKERS_2026-03`
- `CHAIN_MATURITY_GAP_CHECKLIST`
- `GO_NO_GO_STATUS_2026-03-27`
- `P0_P1_P2_EXECUTION_STATUS_2026-03-07`
- `CROSS_STACK_COMPLETENESS_2026-03-08`

## 结论边界

- `Mainnet Bring-up: GO` 不等于 `Public Operations: GO`。
- 能开始受控真实挖矿，不代表已经完成所有公开运营门槛。
- 发布就绪要看的是完整证据链，而不是单个功能点是否能跑。

## 证据怎么用

这个章节的证据分三类：

- 代码与脚本：说明门禁是被实现出来的，不只是写在纸上。
- 文档与状态记录：说明门槛和阶段状态是怎么定义的。
- 报告：说明真实跑过的结果是什么。

## 当前实践

只要涉及版本发布，建议先按这个顺序看：

1. `运行手册`
2. `性能基准`
3. `主网拉起`
4. `发布就绪`

如果最后一章没有过，前面三章都只是“条件满足了部分”，不是可以直接发布的结论。
