# RabbitChain 架构

## 1. 目标

RabbitChain 采用原生 UTXO Compute 路径，统一执行、状态与资源表达，默认签名方案为 `ed25519`。

## 2. 分层结构

```text
┌──────────────────────────────────────────────┐
│ Clients: Wallet / CLI / Explorer / Mining   │
├──────────────────────────────────────────────┤
│ API: HTTP JSON-RPC + WebSocket + REST       │
│      Methods: rabbit_clientVersion /          │
│               rabbit_keccak256 / net_* / rabbit_* │
├──────────────────────────────────────────────┤
│ Core: UTXO Compute / Tx Pool / Block Import │
│       Account / Domain / Object / Policy    │
├──────────────────────────────────────────────┤
│ Storage & Network: StateDB / ComputeStore   │
│                    P2P / PoW / Mining        │
└──────────────────────────────────────────────┘
```

## 3. 交易与执行

- Canonical 写路径：`rabbit_submitComputeTx`
- 查询路径：`rabbit_getComputeTxResult`、`rabbit_getObject`、`rabbit_getOutput`
- 账户查询：`rabbit_getAccount`、`rabbit_getUtxos`
- 区块与挖矿：`rabbit_getLatestBlock`、`rabbit_getWork`、`rabbit_submitWork`

## 4. 签名与地址

- 默认签名：`ed25519`
- 地址格式：`0x...`
- Witness 以原生签名结构提交，按阈值规则进行验证。

## 5. WebSocket

- 订阅：`rabbit_subscribe`
- 取消订阅：`rabbit_unsubscribe`
- 推送事件：`rabbit_subscription`

## 6. 运行时约束

- 默认配置对外暴露 RabbitChain RPC 方法集与网络探针方法。
- 对象状态变更由 Compute 执行器与策略层共同约束。
- 生产配置建议启用鉴权、限流与观测指标。
