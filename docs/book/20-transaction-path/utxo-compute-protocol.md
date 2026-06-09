---
id: compute.protocol
title: UTXO Compute 协议
kind: chapter
status: verified
owner: core-docs
primary_topic: compute.protocol
topics: []
depends_on:
  - tx.validation
  - tx.execution
aliases:
  - utxo.compute.yellowpaper
  - utxo2.0.yellowpaper
evidence:
  - type: code
    ref: crates/rabbitcore/src/compute/tx.rs
  - type: code
    ref: crates/rabbitcore/src/compute/object.rs
  - type: code
    ref: crates/rabbitcore/src/compute/domain.rs
  - type: code
    ref: crates/rabbitcore/src/compute/agent.rs
  - type: code
    ref: crates/rabbitcore/src/compute/scheduler.rs
  - type: code
    ref: crates/rabbitcore/src/compute/execution.rs
  - type: doc
    ref: docs/UTXO-Compute-Yellowpaper-v1.1.md
  - type: doc
    ref: docs/UTXO-2.0-YELLOWPAPER.md
last_reviewed: 2026-06-05
review_due: 2026-07-05
---

# UTXO Compute 协议

这章是把两份历史黄皮书收敛成当前可落地实现口径后的主文档。

## 范围

- 这章讲 Compute 的对象模型、交易模型、验证模型、域与调度模型。
- 这章不把未实现的宏大路线写成既成事实。
- 这章优先使用仓库里已经落地的类型、字段和执行路径来描述。

## 当前对象模型

当前实现的核心对象是版本化输出和 ComputeTx：

- `ObjectOutput` 表示不可变版本输出。
- `ObjectId` 是逻辑对象的稳定标识。
- `OutputId` 是具体物理版本的唯一标识。
- `ComputeTx` 记录输入集、显式读集、输出提案、手续费、过期时间、域、证据和元数据。

### 交易结构

当前 `ComputeTx` 的主字段包括：

- `tx_id`
- `domain_id`
- `command`
- `input_set`
- `read_set`
- `output_proposals`
- `fee`
- `nonce`
- `metadata`
- `payload`
- `deadline_unix_secs`
- `chain_id`
- `network_id`
- `witness`

这比历史黄皮书里的“只是 inputs/outputs 的支付型 UTXO”更明确，因为现在已经把显式读集、域绑定、重放边界和授权见证拆开了。

## 资源与对象

当前资源模型仍然沿着“资源是统一抽象”这个方向，但实现里已经落到了更具体的类型：

- `ResourceValue::Amount`
- `ResourceValue::Data`
- `ResourceValue::Ref`
- `ResourceValue::RefBatch`

对象类别也不是抽象口号，而是已经编码成 `ObjectKind`：

- `Asset`
- `Code`
- `State`
- `Capability`
- `Agent`
- `Anchor`
- `Ticket`

## 域

实现里已经有域注册和域配置：

- `DomainId`
- `DomainConfig`
- `DomainRegistry`

这说明“多域”不是纯路线图，而是已经进入执行与调度边界的实际结构。

## 代理与调度

历史黄皮书里的代理/自治对象，在当前仓库里对应的是：

- `AgentSpec`
- `AgentTask`
- `AgentScheduler`
- `InMemoryAgentScheduler`

它们说明协议里已经保留了“可调度对象”的方向，但目前实现仍然是 scaffold 和内存调度器，不应写成完整链上自治系统已完成。

## 验证边界

Compute 验证现在要同时看：

- 基本结构是否合法
- 输入是否属于正确 domain
- 输出提案是否符合对象/资源约束
- 授权 witness 是否可验证
- `read_set` 是否显式且能参与冲突判断
- 交易 body 是否和 `tx_id` 一致

## 路线图边界

历史黄皮书中的一些设想仍然属于设计空间，不应该写成当前实现事实：

- 完整的自治代理经济闭环
- 全量跨域协议与票据结算
- 更复杂的游戏/市场应用层生态

这些内容可以保留在历史文档里做参考，但当前书库正文只收录已经能被代码和测试支撑的部分。

## 结论

这章是对两份黄皮书的“收敛版”：

- 保留对象化、版本化、显式读集、多域、代理、确定性这些核心思想。
- 删除还没落地的愿景性堆叠。
- 用当前代码里的 `ComputeTx`、`ObjectOutput`、`DomainRegistry`、`AgentScheduler` 这些真实类型来承载描述。
