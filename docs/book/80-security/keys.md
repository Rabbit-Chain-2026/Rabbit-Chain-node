---
id: security.keys
title: 密钥
kind: chapter
status: verified
owner: core-docs
primary_topic: security.keys
topics: []
depends_on: [security.auth]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcli/src/commands/wallet.rs#L768
  - type: test
    ref: crates/rabbitcli/src/commands/wallet.rs#L959
  - type: doc
    ref: docs/KEY_MANAGEMENT_ACCEPTANCE.md
  - type: doc
    ref: docs/CHAIN_WALLET_SECURITY_AUDIT_2026-05-28.md
last_reviewed: 2026-06-09
review_due: 2026-07-09
---

# 密钥

这章讲的是密钥落盘和相关权限控制，不讲钱包 UX。

## 事实

- CLI 写 secret 文件时会走 `write_secret_file(...)`。
- Unix 下该路径创建的文件权限是 `0600`。
- 单测 `saved_secret_files_are_owner_only` 明确验证了这一点。

## 文档边界

- `KEY_MANAGEMENT_ACCEPTANCE.md` 记录的是生产级托管/轮换验收清单。
- `CHAIN_WALLET_SECURITY_AUDIT_2026-05-28.md` 记录的是跨仓库的安全审计计划和结果。

## 结论

密钥章节的重点不是“有没有加密”这么简单，而是：明文不落盘、文件权限受控、访问路径可审计。
