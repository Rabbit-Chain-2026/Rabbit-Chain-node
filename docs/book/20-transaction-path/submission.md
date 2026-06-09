---
id: tx.submission
title: 交易提交
kind: chapter
status: verified
owner: core-docs
primary_topic: transaction.submission
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs
  - type: code
    ref: crates/rabbitapi/src/rpc/compute_adapter.rs
  - type: test
    ref: crates/rabbitapi/src/rpc/mod.rs#L5519
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 交易提交

交易提交的入口是 RPC `rabbit_submitComputeTx`。这条路径的职责很窄：把外部请求转成可执行的 `ComputeTx`，做网络级校验，然后把它交给执行服务。

## 实际路径

1. `rabbit_submitComputeTx` 先解析请求参数，再调用 `parse_compute_tx`。
2. 交易通过 `validate_compute_tx_network` 做网络/链配置校验。
3. `RpcComputeAdapter::submit_compute_tx` 把交易交给 `ComputeExecutionService::submit_and_run`。
4. 执行完成后，RPC 会把结果写入：
   - `submitted_compute_results`
   - `submitted_compute_order`
   - `global_record_compute_tx(...)`
5. 如果配置了持久化存储，还会异步写入持久层，避免重启后结果丢失。

## 这条路径不做什么

- 不做区块打包。
- 不做链上回执生成。
- 不保证一笔交易一定进入某个 block。

## 入口与查询

提交路径和查询路径是分开的：

- 提交：`rabbit_submitComputeTx`
- 仿真：`rabbit_simulateComputeTx`
- 结果查询：`rabbit_getComputeTxResult`

这意味着“提交成功”只说明交易已经进入执行路径，不等价于“已经写进区块”。
