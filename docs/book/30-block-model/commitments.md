---
id: block.commitments
title: 承诺关系
kind: chapter
status: verified
owner: core-docs
primary_topic: block.commitments
topics: []
depends_on: [block.header, block.body]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcore/src/block/mod.rs#L105
  - type: code
    ref: crates/rabbitnet/src/lib.rs#L698
  - type: test
    ref: crates/rabbitapi/src/rpc/mod.rs#L4140
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 承诺关系

`transactions_root` 和 `receipts_root` 不是装饰字段，而是对 body 内容的承诺。

## 怎么算

- `transactions_root` 由 body 里的 `transactions` 计算。
- `receipts_root` 由 body 里的 `receipts` 计算。
- `BlockHeader::reconcile_body_commitments()` 会把 body 算出来的 root 写回 header，或者在不一致时直接报错。

## 怎么校验

- `BlockBody::validate_against_header()` 会检查 tx/receipt 长度是否一致。
- 它还会检查每个 tx 和 receipt 的 id 是否匹配。
- 最后会核对 header 上的 roots 和 body 算出来的 roots 是否一致。

## 存储侧

- body sidecar 入库后，如果 canonical block 已存在，会回写 header 的 root。
- `rabbit_getBlockBody`、`rabbit_getBlockReceipts`、`rabbit_getReceipt` 都基于这套承诺关系返回结果。
