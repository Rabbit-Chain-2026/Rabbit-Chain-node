---
id: storage.block_store
title: 区块存储
kind: chapter
status: verified
owner: core-docs
primary_topic: storage.block_store
topics: []
depends_on: [block.header, block.body]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/lib.rs#L269
  - type: code
    ref: crates/rabbitnet/src/lib.rs#L515
  - type: test
    ref: crates/rabbitnet/src/lib.rs#L2961
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 区块存储

区块存储不是单一表，而是三层协同：

- canonical block snapshot
- body sidecar
- receipt index

## 事实

- `global_store_block(...)` 是 canonical block 的统一入口。
- 如果 block 带 body，它会先校验 commitment，再写入 canonical 缓存。
- 如果 block 本身没有 body，但能够从 body sidecar 找到，也会回填 body。
- `configure_global_block_persistence(...)` 会在启动时加载持久化 block 和 body sidecar。

## 作用

- 支持同步恢复。
- 支持按 number / hash 读取 block。
- 支持 body/receipt 的独立查询。

## 结论

当前实现里的 block 存储不是“只有头”，也不是“只有体”，而是 header/body/receipt 分离但可互相回填。

