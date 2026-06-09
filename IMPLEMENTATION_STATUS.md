# 实现状态

## 核心模块

| 模块 | 状态 | 说明 |
|---|---|---|
| UTXO Compute 执行 | ✅ | Canonical 写路径已可用 |
| 账户与资源模型 | ✅ | `rabbit_getAccount` / `rabbit_getUtxos` 可查询 |
| PoW 与挖矿接口 | ✅ | `rabbit_getWork` / `rabbit_submitWork` 可用 |
| RPC 服务 | ✅ | 默认仅保留 RabbitChain 语义方法 |
| WebSocket 订阅 | ✅ | `rabbit_subscribe` / `rabbit_unsubscribe` |
| 持久化后端 | ✅ | Mem / RocksDB / Redb |

## 最近收敛

- CLI `transaction send` 收敛到 `rabbit_submitComputeTx`
- 钱包端统一 `ed25519`
- 节点 API 入口与文档去旧兼容语义

## 验证建议

```bash
cargo test -p rabbitcli
cargo test -p rabbitapi
```
