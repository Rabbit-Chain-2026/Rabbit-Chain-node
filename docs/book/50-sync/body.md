---
id: sync.body
title: 体同步
kind: chapter
status: verified
owner: core-docs
primary_topic: sync.body
topics: []
depends_on: [sync.headers]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/protocol.rs
  - type: code
    ref: crates/rabbitnet/src/sync.rs#L164
  - type: test
    ref: crates/rabbitnet/src/sync.rs#L1253
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 体同步

头同步之后，节点会按 header hash 拉取 body。

## 协议

- 请求：`SyncGetBlockBody { block_hash }`
- 响应：`SyncBlockBody`

## 校验

体同步必须校验：

- `block_hash` 是否匹配 header
- `tx_count` 是否等于 body 里的交易数
- 交易和回执数量是否一致
- `transactions_root` 和 `receipts_root` 是否和 body 计算值一致
- 如果是 canonical block，body 不能缺失

## 存储语义

- body 可以独立从 sidecar 读取
- body 也可以在 block snapshot 里一并带回
- 对 canonical block，body 不是可有可无的附属品
