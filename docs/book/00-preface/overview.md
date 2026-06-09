---
id: overview
title: 总览
kind: chapter
status: verified
owner: core-docs
primary_topic: overview
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: scripts/docs-check.py
  - type: code
    ref: scripts/docs-build-nav.py
  - type: doc
    ref: docs/manifest.yaml
  - type: doc
    ref: docs/coverage.yaml
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 总览

这本书的目标不是堆文档，而是把 RabbitChain 的实现按可审计的章节拆开。

每一章都只负责一个主题，并且要求满足三件事：

- 章节能独立阅读，不依赖上下文猜测。
- 章节中的事实可以追到代码、测试、日志或报告。
- 不能证明的内容留在 `Open Questions`，不写进正文结论。

## 这本书讲什么

- 交易路径：提交、验证、执行、回执。
- 区块模型：区块头、区块体、承诺关系。
- 同步路径：gossip、头同步、体同步、状态快照、重组。
- 运维路径：标准压测、真实交易测试、真实挖矿 e2e、故障排查。

## 怎么读

- 先看本章，再看对应子章节。
- 每章末尾的 `Evidence` 只列支撑材料，不重复讲故事。
- `Draft` 章节可以先写清范围和现状，再逐步补证据。

## 文档治理

这个书库不是自由散写，而是由 `manifest.yaml`、`coverage.yaml` 和 `docs-check.py` 一起约束：

- `manifest.yaml` 注册章节和顺序。
- `coverage.yaml` 定义必须覆盖的主题。
- `docs-check.py` 校验 frontmatter、证据引用和导航是否一致。
