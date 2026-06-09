---
id: tx.validation
title: 交易验证
kind: chapter
status: verified
owner: core-docs
primary_topic: transaction.validation
topics: []
depends_on: []
aliases: []
evidence:
  - type: code
    ref: crates/rabbitcore/src/compute/execution.rs
  - type: test
    ref: crates/rabbitcore/src/compute/execution.rs#L1079
last_reviewed: 2026-06-04
review_due: 2026-07-04
---

# 交易验证

交易验证发生在纯 compute 层，不依赖区块状态。它决定一笔 `ComputeTx` 是否能被执行。

## 验证顺序

`BasicTxValidator::validate` 的顺序是固定的：

1. `basic_sanity_check`
2. `validate_tx_envelope`
3. 域存在性检查
4. 域是否公开
5. 输入对象检查
6. 读取对象检查
7. 输出提案检查
8. 授权检查
9. 资源检查

## 核心不变量

- 输入对象不能已经 spent。
- 输入对象必须属于当前 domain。
- 读集里的版本号必须和期望版本一致。
- 输出提案的 predecessor 必须存在于 inputs 中。
- predecessor 的 object_id 和版本推进必须正确。
- `Mint` 不能携带非零 fee。
- metadata 不能为空键、重复键、超长键或超长值。

## 结果

验证通过后，执行器拿到的是一个 `ValidationReport`，里面包含已解析的 inputs 和 reads。  
这份报告是后续 `commit_prevalidated` 的输入，不是最终链上回执。
