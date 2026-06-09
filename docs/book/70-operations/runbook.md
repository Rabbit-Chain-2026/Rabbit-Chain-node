---
id: ops.runbook
title: 运行手册
kind: chapter
status: verified
owner: core-docs
primary_topic: ops.runbook
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: scripts/perf_compute_tps.sh
  - type: code
    ref: scripts/mining_e2e.sh
  - type: doc
    ref: docs/COMPUTE_TPS_BENCHMARK_RUNBOOK.md
  - type: doc
    ref: docs/COMPUTE_TPS_BENCHMARK_TUTORIAL.md
  - type: doc
    ref: docs/COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 运行手册

这章是“怎么跑”的标准入口。

## 两条主入口

- `scripts/perf_compute_tps.sh`：compute TPS 标准压测与提交基准。
- `scripts/mining_e2e.sh`：真实挖矿 e2e。

## 记录要求

每次测试都要留档：

- `run.log`
- `meta.txt`
- `report.md`

## 跑法口径

- 本地性能基准看执行和提交路径的稳定性。
- `submit-benchmark` 看真实 RPC 入口吞吐。
- `mining-e2e` 看交易执行、区块高度推进、share 统计和可观测回执。
