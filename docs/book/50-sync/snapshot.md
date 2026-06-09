---
id: sync.snapshot
title: 状态快照
kind: chapter
status: verified
owner: core-docs
primary_topic: sync.snapshot
topics: []
depends_on: [sync.body]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/protocol.rs
  - type: code
    ref: crates/rabbitnet/src/sync.rs#L191
  - type: test
    ref: crates/rabbitnet/src/sync.rs#L1341
  - type: log
    ref: artifacts/mining-e2e/20260604T001819Z/logs/node.log
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 状态快照

状态快照是同步的最后一段，负责把账户和 compute 结果补齐。

## 协议

- 请求：`SyncGetStateSnapshot { block_number }`
- 响应：`SyncStateSnapshot`

## 验证

快照验证不是看“有没有数据”，而是看：

- `state_root` 是否能从账户列表推导出来
- `state_proof` 是否和 block hash 绑定

只有验证通过后，节点才会把 snapshot 里的 accounts 和 compute txs 覆盖到本地状态。

## 作用

- 补齐 body 同步之后仍然缺的状态层数据。
- 修复节点恢复时的本地状态偏差。
