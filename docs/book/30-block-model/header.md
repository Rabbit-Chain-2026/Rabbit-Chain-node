---
id: block.header
title: 区块头
kind: chapter
status: verified
owner: core-docs
primary_topic: block.header
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcore/src/block/mod.rs
  - type: code
    ref: crates/rabbitnet/src/sync.rs
  - type: test
    ref: crates/rabbitnet/src/sync.rs#L1297
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 区块头

区块头是共识的主要承诺对象。body 可以是区块的一部分，但 header 仍然负责：

- 父哈希
- 区块高度
- 时间戳
- 难度
- nonce
- coinbase
- state root
- transactions root
- receipts root
- hash 本身

## 关键实现

- `BlockHeader::compute_hash()` 会按版本区分 legacy 和 canonical preimage。
- `BlockHeader::reconcile_body_commitments()` 会把 body 算出来的 root 和 header 里已有的 root 对齐。
- `BlockHeader::validate()` 继续检查 parent、number、timestamp 和 extra_data 上限。

## 结论

body-first 不等于 header 不重要。  
相反，header 仍然是把 body 承诺进链的对象，只是新模型不再把 body 当成可有可无的附属字段。
