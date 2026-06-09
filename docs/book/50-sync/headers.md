---
id: sync.headers
title: 头同步
kind: chapter
status: verified
owner: core-docs
primary_topic: sync.headers
topics: []
depends_on: [sync.gossip]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/protocol.rs
  - type: code
    ref: crates/rabbitnet/src/sync.rs#L768
  - type: test
    ref: crates/rabbitnet/src/sync.rs#L1341
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 头同步

头同步是链恢复的第一步。它先把“这条链长什么样”对齐，再谈 body 和状态。

## 协议

- 请求：`SyncGetHeaders { start, limit }`
- 响应：`SyncHeaders(Vec<SyncHeader>)`

## 验证顺序

1. 第一个 header 的高度必须等于请求的 `start`。
2. 头之间必须连续。
3. 每个 header 都要和前一个 header 做 parent/number/timestamp/PoW 等校验。
4. 只要第一个 header 的 parent 或 PoW 不对，恢复路径就会回退到更保守的重组探测。

## 作用

- 先找出本地和对端的共同前缀。
- 再决定是 append 还是 reorg。
- 头校验不通过时，不会进入 body 阶段。
