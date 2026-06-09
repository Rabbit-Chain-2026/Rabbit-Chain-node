---
id: foundations.getting_started
title: 快速入门
kind: chapter
status: verified
owner: core-docs
primary_topic: getting.started
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcli/src/main.rs
  - type: code
    ref: crates/rabbitcli/src/commands/account.rs
  - type: code
    ref: crates/rabbitcli/src/commands/compute.rs
  - type: code
    ref: crates/rabbitcli/src/commands/wallet.rs
  - type: doc
    ref: docs/GETTING_STARTED.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 快速入门

这一章吸收旧的 `docs/GETTING_STARTED.md`，目标是给出一条最短路径：把 CLI 跑起来、建钱包、发 compute 交易、查结果。

## 最短路径

1. 构建 `rabbitcli`。
2. 用 `rabbitchain init` 初始化数据目录。
3. 用 `rabbitchain run` 启动本地节点。
4. 用 `rabbitchain account new` 或 `rabbitchain wallet new` 建账户。
5. 用 `rabbitchain compute send` 提交 compute 交易。
6. 用 `rabbitchain compute get` 或 `rabbit_getComputeTxResult` 查询结果。

## 这个入口适合什么

- 本地开发。
- 调 CLI 命令。
- 快速验证 compute 和 RPC 是否打通。

## 这个入口不解决什么

- 不负责完整挖矿联调。
- 不负责多节点同步拓扑。
- 不负责真实压测口径。

如果要跑钱包 + 挖矿 + pool/miner 的闭环，应该继续看 `CLI_WALLET_MINING_TUTORIAL.md` 和 `运行手册`。
