---
id: tx.receipt
title: 交易回执
kind: chapter
status: verified
owner: core-docs
primary_topic: transaction.receipt
topics: []
depends_on: [tx.execution]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs
  - type: report
    ref: artifacts/mining-e2e/20260604T001819Z/report.md
  - type: test
    ref: crates/rabbitapi/src/rpc/mod.rs#L4140
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 交易回执

这套实现里要分清两个概念：

- `compute result`
- `chain receipt`

## Compute result

`rabbit_getComputeTxResult` 查的是执行服务保存的结果。  
它来自提交时的返回值、内存缓存或持久化存储，能说明“这笔交易执行过并产出过结果”。

## Chain receipt

`rabbit_getReceipt` 和 `rabbit_getBlockReceipts` 查的是链上/区块体里的回执语义。  
它们依赖 `BlockBody` 和 `Receipt`，不是 compute result 的别名。

## 当前观测

真实挖矿 e2e 里，`rabbit_getComputeTxResult` 是 `ok=true`，但 `rabbit_getReceipt` 仍可能是 `null`。  
这说明当前链路已经能把执行结果和链上回执拆开观测，但链上回执还没有完全 materialize。
