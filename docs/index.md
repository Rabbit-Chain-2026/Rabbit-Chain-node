# RabbitChain Handbook

这是一套证据驱动的仓库内文档站。正文按章节拆分，每章只讲一个主题，并且要求能追到代码、测试、日志或报告。

## 从这里开始

- [总览](book/00-preface/overview.md)
- [设计原则](book/00-preface/design-principles.md)
- [工程红线](book/00-preface/engineering-redlines.md)
- [API 参考](book/10-foundations/api.md)
- [快速入门](book/10-foundations/getting-started.md)
- [交易提交](book/20-transaction-path/submission.md)
- [交易验证](book/20-transaction-path/validation.md)
- [交易执行](book/20-transaction-path/execution.md)
- [交易回执](book/20-transaction-path/receipt.md)
- [Compute JSON 规范](book/20-transaction-path/compute-json.md)
- [UTXO Compute 协议](book/20-transaction-path/utxo-compute-protocol.md)
- [区块头](book/30-block-model/header.md)
- [区块体](book/30-block-model/body.md)
- [承诺关系](book/30-block-model/commitments.md)
- [P2P 传输](book/40-network/p2p-transport.md)
- [区块存储](book/40-storage/block-store.md)
- [区块体存储](book/40-storage/body-store.md)
- [回执索引](book/40-storage/receipt-index.md)
- [挖矿流程](book/60-mining/mining-flow.md)
- [减半时间表](book/60-mining/halving-schedule.md)
- [头同步](book/50-sync/headers.md)
- [体同步](book/50-sync/body.md)
- [状态快照](book/50-sync/snapshot.md)
- [重组](book/50-sync/reorg.md)
- [运行手册](book/70-operations/runbook.md)
- [发布就绪](book/70-operations/release-readiness.md)
- [主网拉起](book/70-operations/mainnet-bringup.md)
- [Workspace 验收](book/70-operations/workspace-acceptance.md)
- [性能基准](book/70-operations/benchmarks.md)
- [故障排查](book/70-operations/troubleshooting.md)
- [RPC 认证](book/80-security/auth.md)
- [密钥](book/80-security/keys.md)
- [审计](book/80-security/audits.md)
- [旧文档索引](book/95-legacy/archive.md)

## 读法

建议顺序是：

1. 先看 [总览](book/00-preface/overview.md)，了解这本书的范围和证据规则。
2. 再看 [设计原则](book/00-preface/design-principles.md) 和 [工程红线](book/00-preface/engineering-redlines.md)，确认这套实现的边界和禁止项。
3. 接着看 [交易路径](book/20-transaction-path/execution.md) 和 [UTXO Compute 协议](book/20-transaction-path/utxo-compute-protocol.md)，确认交易、对象、读集和域边界。
4. 然后看 [区块模型](book/30-block-model/commitments.md) 和 [P2P 传输](book/40-network/p2p-transport.md)，理解区块承诺和节点如何连。
5. 再看 [同步](book/50-sync/headers.md) 和 [存储](book/40-storage/block-store.md)，理解节点如何接收、落盘和恢复。
6. 最后看 [运行手册](book/70-operations/runbook.md) 与 [发布就绪](book/70-operations/release-readiness.md)，把操作口径、门禁和历史资料对齐。

## 这本书的规则

- 每章独立更新。
- 每章的事实都要可追证据。
- 不能证明的内容放到 `Open Questions` 或 `legacy`，不要写成结论。
- 新内容优先写进 book 章节，旧内容只保留为历史参考或迁移索引。

## 章节结构

- `00-preface`: 总览、设计原则、工程红线
- `10-foundations`: API 参考、快速入门
- `20-transaction-path`: 交易提交、验证、执行、回执
- `30-block-model`: 区块头、区块体、承诺关系
- `40-network`: P2P 传输
- `40-storage`: 区块、区块体、回执存储
- `60-mining`: 挖矿流程、减半时间表
- `50-sync`: gossip、头同步、体同步、状态快照、重组
- `70-operations`: 运行手册、发布就绪、主网拉起、Workspace 验收、性能基准、故障排查
- `80-security`: 认证、密钥、审计

## 旧文档

历史资料没有直接删掉，而是集中在 [旧文档索引](book/95-legacy/archive.md) 里做去向表。  
如果某份旧文档已经被新章节吸收，以 book 章节为准；如果还没吸收，就继续保留在 legacy 参考区。
