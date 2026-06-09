---
id: security.auth
title: RPC 认证
kind: chapter
status: verified
owner: core-docs
primary_topic: security.auth
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs#L2370
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs#L2442
  - type: test
    ref: crates/rabbitapi/src/rpc/mod.rs#L5303
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# RPC 认证

节点的 RPC 认证是显式 token 认证，不是“看起来像本地就放行”。

## 规则

- 状态写方法需要 token 配置。
- token 可以通过 `Authorization: Bearer ...` 或 `x-rabbit-token` 传入。
- 认证失败返回 `Unauthorized`。

## 写方法

当前需要 token 的写方法包括：

- `rabbit_submitComputeTx`
- `rabbit_submitWork`
- `rabbit_importBlock`

## 结论

安全口径很明确：只要是 stateful write，就必须过 token gate。

