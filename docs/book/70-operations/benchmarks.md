---
id: ops.benchmarks
title: 性能基准
kind: chapter
status: verified
owner: core-docs
primary_topic: ops.benchmarks
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: scripts/perf_compute_tps.sh
  - type: test
    ref: crates/rabbitapi/src/rpc/mod.rs#L5519
  - type: report
    ref: docs/COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 性能基准

compute 性能基准分两类：

- 本地标准压测：用于观察代码变更是否退化。
- 真实交易测试：用于观察真实 RPC 入口的提交吞吐。

## 标准口径

- 所有测试都必须留档。
- 默认样本量是 `1,000,000` 交易。
- 真实交易测试用 `submit-benchmark`。
- 多节点 round-robin 时，优先看入口层是否能稳定承载，而不是只看单次峰值。

## 怎么看结果

- `submit-benchmark` 的 TPS 是同步等待提交结果的吞吐，不等于 block 容量。
- `mining-e2e` 的重点不是 TPS，而是能不能观测到执行成功、出块推进和 share 统计。
