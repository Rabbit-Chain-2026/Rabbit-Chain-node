---
id: ops.troubleshooting
title: 故障排查
kind: chapter
status: verified
owner: core-docs
primary_topic: ops.troubleshooting
topics: []
depends_on: []
aliases: []
evidence:
  - type: log
    ref: artifacts/mining-e2e/20260604T001819Z/logs/node.log
  - type: log
    ref: artifacts/mining-e2e/20260604T001819Z/logs/miner.log
  - type: report
    ref: artifacts/mining-e2e/20260604T001819Z/report.md
  - type: test
    ref: crates/rabbitnet/src/sync.rs#L1341
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 故障排查

排障时先看三类症状：

- 执行结果有没有回来。
- 区块高度有没有推进。
- share / metrics 有没有增长。

## 常见问题

- `rabbit_getComputeTxResult` 有值，但 `rabbit_getReceipt` 还是 `null`：说明执行结果已经出来，但链上回执还没有 materialize。
- `submit-benchmark` TPS 偏低：优先检查并发是否太低、是否热编译、是否有资源争用。
- `mining-e2e` 没有 height 增长：先看 node 是否真的 `Mining: enabled`，再看 pool/miner 是否连上。
- 同一笔交易重复 fallback：优先查 `submit_and_run` 的 in-flight 和 completed 保护是否生效。

## 排障原则

- 先看日志，再看报告，再回到代码。
- 不能直接把“没看到结果”解释成“代码一定错了”。
- 不能把“本地受控节点的现象”直接外推成全网结论。
