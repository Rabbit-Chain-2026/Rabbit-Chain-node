---
id: storage.receipt_index
title: 回执索引
kind: chapter
status: verified
owner: core-docs
primary_topic: storage.receipt_index
topics: []
depends_on: [storage.body_store]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/lib.rs#L588
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs#L1600
  - type: test
    ref: crates/rabbitapi/src/rpc/mod.rs#L4140
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 回执索引

回执索引是从 tx hash 到 receipt 的查询层。

## 查询入口

- `global_block_receipt_by_tx_hash(...)`
- `global_block_receipts_by_hash(...)`
- `rabbit_getReceipt`
- `rabbit_getBlockReceipts`

## 语义

- `rabbit_getReceipt` 是按交易 hash 查单条回执。
- `rabbit_getBlockReceipts` 是按 block hash 查整块回执数组。
- 这些接口返回的是链上/区块体回执，不是 compute result 的别名。

## 结论

回执索引让“按交易查执行结果”这件事从 compute 结果缓存，扩展到了链上数据视图。

