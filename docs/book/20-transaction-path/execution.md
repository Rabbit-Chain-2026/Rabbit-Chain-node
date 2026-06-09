---
id: tx.execution
title: 交易执行
kind: chapter
status: verified
owner: core-docs
primary_topic: transaction.execution
topics: []
depends_on: [tx.validation]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcore/src/compute/batch.rs
  - type: code
    ref: crates/rabbitcore/src/compute/execution.rs
  - type: test
    ref: crates/rabbitcore/src/compute/batch.rs#L1234
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 交易执行

执行路径的目标不是“快就完了”，而是“在并发下不重复执行、在回退时不双重提交”。

## 调用链

`rabbit_submitComputeTx` 进入执行服务后，大体会经历：

1. `submit_and_run`
2. `inflight_entry`
3. `submit`
4. 批窗口等待
5. `flush_ready`
6. `planner.plan` / `runner.run_plan`
7. 必要时退回 `runner.run_serial`
8. `record_outcomes`
9. `complete_inflight`

## 并发保护

- 同一个 `tx_id` 只允许一个 in-flight leader。
- 后来的并发调用会等 leader 的结果，不会自己再走一遍串行 fallback。
- 批窗口和稳定等待之间会再确认一次结果是否已经落地。
- 如果 `completed` 已经有结果，调用会直接短路返回。

## 执行器

`BasicTxExecutor::execute` 的顺序是：

1. 先验证。
2. 再 `commit_prevalidated`。
3. `commit_prevalidated` 里才真正把 inputs 标记为 spent，并插入 outputs。

这意味着执行和验证是分开的，验证通过并不等于状态已经写入；状态写入发生在 commit 阶段。
