---
id: sync.reorg
title: 重组
kind: chapter
status: verified
owner: core-docs
primary_topic: sync.reorg
topics: []
depends_on: [sync.headers, sync.body]
aliases: []
evidence:
  - type: code
    ref: crates/rabbitnet/src/lib.rs
  - type: code
    ref: crates/rabbitnet/src/sync.rs#L583
  - type: test
    ref: crates/rabbitnet/src/lib.rs#L361
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 重组

重组不是“发现分叉就全盘推倒”，而是 suffix replacement。

## 行为

1. 找到本地链和对端链的第一次分歧高度。
2. 把分歧高度之后的 canonical suffix 切掉。
3. 用对端验证过的 blocks 替换尾段。
4. 如果新块需要 body，而 body 不存在，就拒绝这次恢复。

## 相关实现

- `global_replace_block_chain(...)` 负责替换 canonical 尾段。
- `validate_global_block_chain_replacement(...)` 负责替换前校验。
- `validate_persisted_block_chain(...)` 负责启动时和 genesis/height-1 起点的链校验。

## 结论

reorg 仍然是 header-led 的，但 canonical block 已经要求 body-aware。也就是说，头决定链段，体决定内容一致性。
