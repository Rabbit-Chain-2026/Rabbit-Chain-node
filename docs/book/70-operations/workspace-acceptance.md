---
id: ops.workspace_acceptance
title: Workspace 验收
kind: chapter
status: verified
owner: core-docs
primary_topic: ops.workspace_acceptance
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: scripts/workspace_acceptance.sh
  - type: doc
    ref: docs/WORKSPACE_ACCEPTANCE_CHECKLIST.md
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# Workspace 验收

这一章吸收 workspace acceptance 的统一口径。它不是单仓检查，而是跨仓检查：

- `Rabbit-Chain-node`
- `rabbitchain-explorer`
- `rabbitchain-mining-stack`
- `rabbitchain-wallet-chrome`
- `rabbitchain-wallet-mobile`

## 主入口

- 快速模式：`bash scripts/workspace_acceptance.sh --quick`
- 完整模式：`bash scripts/workspace_acceptance.sh --full`

## 关注点

- 计算 JSON fixture 跨仓一致性
- 钱包前端构建和测试
- 挖矿、同步、explorer 的基础联通
- 相关文档和脚本是否仍然能互相对上
