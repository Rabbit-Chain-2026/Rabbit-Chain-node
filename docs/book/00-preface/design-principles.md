---
id: design.principles
title: 设计原则
kind: chapter
status: verified
owner: core-docs
primary_topic: design.principles
topics: []
depends_on:
  - overview
aliases:
  - design.philosophy
evidence:
  - type: code
    ref: crates/rabbitapi/src/rpc/mod.rs
  - type: code
    ref: crates/rabbitcli/src/main.rs
  - type: code
    ref: crates/rabbitcore/src/compute/execution.rs
  - type: doc
    ref: docs/DESIGN_PHILOSOPHY.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 设计原则

这章只讲当前实现已经落地的设计原则，不讲抽象口号。

## 范围

- 这章讲协议和工程为什么这样设计。
- 这章不重复区块模型、同步协议和发布门禁的细节。
- 不能从代码和文档同时支撑的说法，不写进结论。

## 核心原则

### 默认接口必须显式

RabbitChain 对外默认暴露的是 `rabbit_*` 方法集，而不是把链内语义藏在别的抽象里。
这样做的目标是让调用方直接看见交易提交、结果查询、挖矿和块查询这些主路径。

### 执行必须可验证、可重复

交易执行不是“跑通就行”，而是要在同样输入下得到同样结果。
执行路径、验证路径和结果记录路径分开处理，避免把状态写入和结果观测混成一件事。

### 更新必须通过版本表达

协议里真正被承诺和恢复的对象是版本化结果，而不是一个可随意原地修改的共享状态。
这也是为什么新写入更强调“消费旧版本、创建新版本”的表达方式。

### 兼容必须显式，不能靠默认回退

兼容逻辑如果存在，必须是明确开关，并且默认关闭。
默认行为应该是最可审计、最不容易掩盖故障的路径。

## 本章和其他章节的边界

- `交易路径` 讲的是怎么提交、验证和执行一笔交易。
- `工程红线` 讲的是哪些实现方式不能碰。
- `发布就绪` 讲的是能不能上线、缺哪些门禁。

## 结论

设计原则不是“写在文档里的愿望”，而是要能在代码、脚本和门禁里找到对应物。
没有对应物的原则，只能放进历史背景或待办，不应写成当前事实。
