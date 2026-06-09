---
id: security.audits
title: 审计
kind: chapter
status: verified
owner: core-docs
primary_topic: security.audits
topics: []
depends_on: [security.auth, security.keys]
aliases: []
evidence:
  - type: doc
    ref: docs/CHAIN_WALLET_SECURITY_AUDIT_2026-05-28.md
  - type: doc
    ref: docs/AUDIT_2026-03-16.md
  - type: doc
    ref: docs/KEY_MANAGEMENT_ACCEPTANCE.md
  - type: test
    ref: crates/rabbitcli/src/commands/wallet.rs#L959
last_reviewed: 2026-06-09
review_due: 2026-07-09
---

# 审计

审计章只负责记录“有什么审计、审计到了哪一步、结果是什么”，不在这里重复结论。

## 已有材料

- 密钥托管与轮换验收清单。
- 链与钱包安全审计计划与结果。
- 代码审计/文档补齐记录。

## 作用

- 给安全决策留证据。
- 给后续变更提供历史锚点。
- 让安全章节可持续更新，而不是一次性报告。
