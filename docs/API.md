# RabbitChain API

## Endpoints

- HTTP JSON-RPC: `http://127.0.0.1:8545`
- WebSocket: `ws://127.0.0.1:8546`

## 方法分组

- 信息与网络：`rabbit_clientVersion`、`rabbit_keccak256`、`net_*`
- RabbitChain 扩展：`rabbit_*`

## 常用 RPC 示例

### 1) 查询账户

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getAccount","params":["0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9"],"id":1}'
```

### 2) 查询最新区块

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getLatestBlock","params":[],"id":1}'
```

### 3) 领取矿工工作

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getWork","params":[],"id":1}'
```

### 4) 提交矿工工作

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_submitWork","params":[{"work_id":"work-...","nonce":7,"hash_hex":"0x...","miner":"miner-1"}],"id":1}'
```

### 5) 提交 Compute 操作

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_submitComputeTx","params":[{"tx_id":"0x...","domain_id":0,"command":"Transfer","input_set":[],"output_proposals":[],"witness":{"signatures":[{"scheme":"ed25519","public_key":"0x...","signature":"0x..."}]}}],"id":1}'
```

### 6) 查询 Compute 操作结果

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getComputeTxResult","params":["0x..."],"id":1}'
```

### 7) 旧 transfer RPC 已移除

余额变动与对象更新统一通过 compute 交易完成；节点不再提供 `rabbit_transfer`。

### 8) 按高度查询区块（新增）

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getBlockByNumber","params":["0x2a"],"id":1}'
```

### 9) 区块范围查询（新增）

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getBlocksRange","params":[{"from":"0x1","to":"0x64","limit":50}],"id":1}'
```

### 10) 最近 Compute 操作列表（新增）

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_listComputeTxResults","params":[{"page":1,"limit":20}],"id":1}'
```

### 11) 按哈希查询操作（新增）

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getOperationByHash","params":["0x..."],"id":1}'
```

### 12) 最近操作列表（新增，compute-only）

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_listOperations","params":[{"page":1,"limit":20,"kind":"all"}],"id":1}'
```

### 13) 按地址查询操作列表（新增）

```bash
curl -X POST http://127.0.0.1:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getOperationsByAddress","params":[{"address":"0x...","page":1,"limit":20}],"id":1}'
```

当前节点会返回 `unsupported: true`，因为旧 transfer 索引已移除，地址级操作历史尚未在 compute-only 路径下重建。

## WebSocket 订阅

### 建立订阅

```json
{"jsonrpc":"2.0","method":"rabbit_subscribe","params":["newHeads"],"id":1}
```

### 取消订阅

```json
{"jsonrpc":"2.0","method":"rabbit_unsubscribe","params":["<subscription-id>"],"id":2}
```

### 事件推送格式

- `method`: `rabbit_subscription`
- `params.subscription`: 订阅 ID
- `params.result`: 事件内容

## 错误码

| code | 含义 |
|---|---|
| -32700 | Parse error |
| -32600 | Invalid Request |
| -32601 | Method not found |
| -32602 | Invalid params |
| -32603 | Internal error |
