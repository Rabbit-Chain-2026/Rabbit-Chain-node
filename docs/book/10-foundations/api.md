---
id: foundations.api
title: API 参考
kind: chapter
status: verified
owner: core-docs
primary_topic: api.reference
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs
  - type: code
    ref: crates/rabbitcli/src/commands/rpc.rs
  - type: doc
    ref: docs/API.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# API 参考

这一章吸收旧的 `docs/API.md`，但口径改成了“以实现为准”的章节写法。它只回答两个问题：

1. 节点公开了哪些 RPC / CLI 入口。
2. 哪些入口需要 token，哪些只是读取型查询。

## 方法分组

RabbitChain 的 RPC 层可以粗分成两组：

- 信息与网络方法，例如 `rabbit_clientVersion`、`rabbit_keccak256`、`net_*`
- RabbitChain 扩展方法，例如 `rabbit_getAccount`、`rabbit_getLatestBlock`、`rabbit_getWork`、`rabbit_submitWork`、`rabbit_submitComputeTx`

## 写方法边界

当前的写方法不会裸放行，至少这些入口需要 token gate：

- `rabbit_submitComputeTx`
- `rabbit_submitWork`
- `rabbit_importBlock`

这意味着“RPC 可达”不等于“写入可用”。写入和读取是分开的。

## 常用查询

最常被调用的查询入口包括：

- `rabbit_getAccount`
- `rabbit_getLatestBlock`
- `rabbit_getBlockByNumber`
- `rabbit_getBlocksRange`
- `rabbit_getComputeTxResult`
- `rabbit_listComputeTxResults`
- `rabbit_getOperationByHash`
- `rabbit_listOperations`
- `rabbit_getOperationsByAddress`

## CLI 关系

`rabbitchain` 的 CLI 不是另起一套 API，而是把这些 JSON-RPC 入口包装成命令行子命令。  
所以这章既能解释 HTTP/WS 调用方式，也能解释 CLI 背后实际打到哪些 RPC 方法。
