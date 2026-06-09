---
id: sync.gossip
title: Gossip
kind: chapter
status: verified
owner: core-docs
primary_topic: sync.gossip
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/lib.rs#L1441
  - type: code
    ref: crates/rabbitnet/src/sync.rs#L219
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# Gossip

gossip 只负责“通知”，不负责链恢复。

## 现有广播

- `broadcast_compute_tx(tx_hash)` 发送新的 compute tx 哈希。
- `broadcast_block(block)` 发送新的 block。

## 作用边界

- 它能让对等节点尽快知道有新消息。
- 它不能代替 header-first 同步。
- 它不能代替 body 拉取。
- 它也不能代替 state snapshot 修复。

## 结论

在当前实现里，gossip 是入口层扩散，不是完整同步协议。
