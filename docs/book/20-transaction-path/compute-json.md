---
id: tx.compute_json
title: Compute JSON 规范
kind: chapter
status: verified
owner: core-docs
primary_topic: compute.json
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs
  - type: code
    ref: crates/rabbitcore/src/compute/tx.rs
  - type: doc
    ref: docs/COMPUTE_JSON_SPEC.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# Compute JSON 规范

这一章吸收旧的 `docs/COMPUTE_JSON_SPEC.md`，定义 compute 交易的共享 JSON 规范。

它的作用是统一这些入口的对象形状：

- `rabbitchain compute send`
- `rabbit_simulateComputeTx`
- `rabbit_submitComputeTx`
- `rabbitchain-wallet-chrome`
- `rabbitchain-wallet-mobile`

## 单一事实来源

这份规范不是凭空写的，真正的来源只有两处：

- RPC 解析与校验：`crates/rabbitapi/src/rpc/mod.rs`
- signing preimage：`crates/rabbitcore/src/compute/tx.rs`

## 这章的定位

- 解释字段含义和默认值。
- 解释哪些字段会参与签名。
- 解释不同客户端如何归一化同一份 compute 对象。

## 这章不做什么

- 不重复讲执行调度。
- 不重复讲 block / receipt。
- 不重复讲钱包 UI 细节。
