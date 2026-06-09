---
id: storage.body_store
title: 区块体存储
kind: chapter
status: verified
owner: core-docs
primary_topic: storage.body_store
topics: []
depends_on: [storage.block_store]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/lib.rs#L584
  - type: code
    ref: crates/rabbitnet/src/lib.rs#L614
  - type: test
    ref: crates/rabbitnet/src/lib.rs#L2998
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 区块体存储

区块体单独存成 sidecar，是为了让 canonical block 既能按头同步，也能按体独立读取。

## 关键路径

- `global_block_body_by_hash(...)`：按 block hash 读取 body。
- `global_block_body_by_number(...)`：先按 number 找 block，再取 body。
- `store_block_body(...)`：写入 body sidecar，并刷新 receipt 索引。

## 校验

- body 的版本必须被支持。
- tx/receipt 数量必须一致。
- 每个 tx 与 receipt 的 id 必须对齐。
- receipt 的 block hash 必须匹配对应 block。

## 结论

体存储不是缓存而已。它是 canonical block 的一部分，只是为了兼容同步和查询被拆成 sidecar。

