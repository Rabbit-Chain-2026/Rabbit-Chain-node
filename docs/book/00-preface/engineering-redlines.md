---
id: engineering.redlines
title: 工程红线
kind: chapter
status: verified
owner: core-docs
primary_topic: engineering.redlines
topics: []
depends_on:
  - design.principles
aliases:
  - engineering.redline
evidence:
  - type: code
    ref: scripts/no_silent_fallback.sh
  - type: code
    ref: scripts/run_tests.sh
  - type: code
    ref: .github/workflows/redline-guard.yml
  - type: doc
    ref: docs/ENGINEERING_REDLINES.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 工程红线

这章讲的是“不能碰什么”，不是“建议怎么做”。

## 红线清单

### 默认 fail-fast

配置非法、存储后端不可用、协议载荷非法时，必须尽早失败。
不能为了继续跑下去就偷偷换成别的默认路径。

### 禁止静默吞错

关键链路的错误不能只记 warning 然后继续正常运行。
如果某个路径已经不可用，就必须把失败暴露出来。

### 兼容开关必须显式

如果一定要保留旧路径，它必须是明确开关，默认关闭。
默认开启的兼容回退，等价于把故障藏起来。

### 每个修复必须带反向测试

修复不是改完就结束。
必须补一个能证明旧行为会掩盖问题、新行为会显式失败的测试。

## 自动门禁

- `scripts/no_silent_fallback.sh` 负责扫静默 fallback 语义。
- `scripts/run_tests.sh` 负责把 redline guard 放进发布门禁。
- `.github/workflows/redline-guard.yml` 负责把这条约束接进 CI。

## 例外机制

如果某处真的需要例外，必须显式标注 `REDLINE_ALLOW`，并写清原因。
没有理由注释的例外，不算例外，只算违规。

## 结论

工程红线不是“风格偏好”，而是主干质量的一部分。
如果一条路径需要静默回退才能工作，那条路径就还没有准备好进入主干。
