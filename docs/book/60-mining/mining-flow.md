---
id: mining.flow
title: 挖矿流程
kind: chapter
status: verified
owner: core-docs
primary_topic: mining.flow
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcli/src/commands/wallet.rs
  - type: code
    ref: crates/rabbitcli/src/main.rs
  - type: code
    ref: scripts/mining_e2e.sh
  - type: doc
    ref: docs/CLI_WALLET_MINING_TUTORIAL.md
last_reviewed: 2026-06-09
review_due: 2026-07-09
---

# 挖矿流程

这一章吸收旧的 `docs/CLI_WALLET_MINING_TUTORIAL.md` 中最常用的主线：钱包、coinbase、节点、pool/miner、真实挖矿闭环。

## 主线

1. 先创建或解锁 CLI 钱包。
2. 把钱包地址设成 coinbase。
3. 启动节点，确保 RPC 已经带上认证 token。
4. 选择内置矿工或外部 `rabbitchain-mining-stack`。
5. 用 `scripts/mining_e2e.sh` 记录交易提交、执行结果、区块高度和 share。

## 这章的关注点

- 钱包如何创建、查看、签名和解锁。
- coinbase 如何接到节点。
- 本地矿工和外部 pool/miner 的区别。
- 哪些结果要留档。

## 这章不展开什么

- 不重复讲完整 RPC 认证细节。
- 不重复讲 compute 执行内部路径。
- 不重复讲同步协议，只在需要时引用对应章节。

## 配套文档

- [减半时间表](halving-schedule.md)：可直接填 `T0` 的独立模板。
