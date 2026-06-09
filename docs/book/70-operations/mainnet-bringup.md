---
id: ops.mainnet_bringup
title: 主网拉起
kind: chapter
status: verified
owner: core-docs
primary_topic: ops.mainnet_bringup
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: scripts/mainnet_local_bringup.sh
  - type: code
    ref: scripts/mainnet.sh
  - type: doc
    ref: docs/MAINNET_BRINGUP_RUNBOOK.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 主网拉起

这一章吸收主网 bring-up 的主线：bootnode、follower、observer、pool、miner、explorer 如何按顺序拉起。

## 主线

1. 先起 `bootnode / coordinator`。
2. 再起 `follower` 和 `observer`。
3. 再接外部 `pool + miner`。
4. 再起 explorer backend。

## 关键观察项

- `rabbit_getWork` 是否可用。
- `rabbit_submitWork` 是否 accepted。
- 区块高度是否推进。
- pool / miner share 是否增长。

## 这章适合什么

- 受控主网启网。
- 小范围白名单节点 / 白名单矿工 bring-up。
- 需要按步骤检查联通性的场景。
