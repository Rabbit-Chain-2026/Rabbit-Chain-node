---
id: block.body
title: 区块体
kind: chapter
status: verified
owner: core-docs
primary_topic: block.body
topics: []
depends_on: [block.header]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcore/src/block/mod.rs
  - type: code
    ref: crates/rabbitnet/src/lib.rs
  - type: test
    ref: crates/rabbitnet/src/lib.rs#L2998
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 区块体

区块体是 canonical block 的一等公民。当前实现里它通过 `BlockBody` 表示，里面有两类内容：

- `transactions`
- `receipts`

## 语义

- `transactions` 是按顺序排列的 canonical transaction envelope。
- `receipts` 是和交易一一对应的执行回执。
- body 自带版本号，当前 P1/P2/P3/P4 迁移阶段里先用固定版本做兼容。

## 约束

- 交易数和回执数必须一致。
- 每个 `tx_id` 必须和对应 receipt 的 `tx_id` 一致。
- receipt 的 `block_hash` 必须等于 header hash。
- body 可以独立存储，也可以和 header 一起存。

## 结论

当前不是“body 只是附加数据”，而是“header 对 body 做 commitment，body 本身可被读、可被索引、可被同步”。
