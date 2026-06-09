# Compute JSON Spec

这份文档定义 RabbitChain `compute` JSON 的共享规范。

适用范围：
- `rabbitchain compute send`
- `rabbit_simulateComputeTx`
- `rabbit_submitComputeTx`
- `rabbitchain-wallet-chrome`
- `rabbitchain-wallet-mobile`

单一事实来源：
- RPC 解析与校验：`crates/rabbitapi/src/rpc/mod.rs`
- signing preimage：`crates/rabbitcore/src/compute/tx.rs`

## 顶层结构

最小提交对象必须是一个 JSON object：

```json
{
  "tx_id": "0x...",
  "domain_id": 0,
  "command": "Mint",
  "input_set": [],
  "read_set": [],
  "output_proposals": [],
  "fee": 0,
  "nonce": 1,
  "metadata": [],
  "payload": "0x",
  "deadline_unix_secs": null,
  "chain_id": 10086,
  "network_id": 10086,
  "witness": {
    "threshold": 1,
    "signatures": []
  }
}
```

## 字段规则

### `tx_id`
- 类型：`0x` 前缀 32-byte hex string。
- 语义：必须等于 `keccak256(signing_preimage)`。
- CLI 行为：`rabbitchain compute send` 提交前会自动重算并覆盖错误的 `tx_id`。
- 钱包行为：插件钱包和移动钱包签名时会自动生成正确 `tx_id`。

### `domain_id`
- 类型：`u32`。
- 当前常用值：`0`。

### `command`
- 允许值：
  - `Transfer`
  - `Invoke`
  - `Mint`
  - `Burn`
  - `Anchor`
  - `Reveal`
  - `AgentTick`

### `input_set`
- 类型：32-byte hash 数组。
- `Transfer` / `Invoke` / `Burn` 一般需要非空。

### `read_set`
- 类型：数组。
- 元素结构：

```json
{
  "output_id": "0x...",
  "domain_id": 0,
  "expected_version": 1
}
```

### `output_proposals`
- 类型：数组。
- 元素结构：

```json
{
  "output_id": "0x...",
  "object_id": "0x...",
  "domain_id": 0,
  "kind": "Asset",
  "owner": { "type": "Shared" },
  "predecessor": null,
  "version": 1,
  "state": "0x",
  "state_root": null,
  "resources": [],
  "lock": { "vm": 1, "code": "0x" },
  "logic": null,
  "created_at": 0,
  "ttl": null,
  "rent_reserve": null,
  "flags": 0,
  "extensions": []
}
```

### `kind`
- 允许值：
  - `Asset`
  - `Code`
  - `State`
  - `Capability`
  - `Agent`
  - `Anchor`
  - `Ticket`

### `owner`
- 缺省时默认等价于：

```json
{ "type": "Shared" }
```

- 支持形式：

```json
{ "type": "Shared" }
```

```json
{ "type": "Address", "address": "0x9aea038CD4255BaaC26eAC5A74e58a07ED2f1975" }
```

```json
{ "type": "Program", "address": "0x9aea038CD4255BaaC26eAC5A74e58a07ED2f1975" }
```

```json
{ "type": "Ed25519", "public_key": "0xea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c" }
```

#### `Address` / `Program`
- 规范口径是 `0x...`。
- RPC 解析接受 `0x...`。
- 钱包内部在做 signing preimage 编码时会提取为 20-byte 地址。
- 不要把 `address` 字段手工改成 32-byte hash、公钥或其它编码。

#### `Ed25519`
- 规范口径是 32-byte `0x...` hex。
- 钱包实现会容忍重复前缀如 `0x0x...` 并归一化为单个 `0x`。

### `predecessor`
- 类型：`0x...` 32-byte hash 或 `null`。
- `Transfer` / 更新类输出通常应指向被替代的旧输出。

### `version`
- 类型：`u64`。
- 新对象通常从 `1` 开始。
- `Transfer` 更新通常递增，例如 `1 -> 2`。

### `state`
- 类型：hex bytes，默认可为空 `0x`。

### `state_root`
- 类型：32-byte hash 或 `null`。

### `resources`
- 类型：数组。
- 元素结构：

```json
{
  "asset_id": "0x...",
  "value": {
    "type": "Amount",
    "amount": "0x2a"
  }
}
```

支持的 `value.type`：
- `Amount`
- `Data`
- `Ref`
- `RefBatch`

`asset_id` 必须唯一，节点会按 `asset_id` 排序和校验。

### `lock`
- 类型：object 或缺省。
- 缺省时等价于：

```json
{ "vm": 1, "code": "0x" }
```

### `logic`
- 类型：object 或 `null`。

### `created_at`
- 类型：`u64`，默认 `0`。

### `ttl`
- 类型：`u64` / hex string / `null`。

### `rent_reserve`
- 类型：`u128` / hex string / `null`。

### `flags`
- 类型：`u32`，默认 `0`。

### `extensions`
- 类型：数组。
- 元素结构：

```json
{
  "key": "note",
  "value": "0x68656c6c6f"
}
```

### `fee`
- 类型：`u64` / hex string。

### `nonce`
- 类型：`u64` / hex string / `null`。

### `metadata`
- 类型：数组。
- 元素结构与 `extensions` 相同。

### `payload`
- 类型：hex bytes 或 `null`，缺省等价于 `0x`。

### `deadline_unix_secs`
- 类型：`u64` / hex string / `null`。

### `chain_id`
- 类型：`u64` / hex string / `null`。
- 当前主网口径：`10086`。

### `network_id`
- 类型：`u32` / decimal / `null`。
- 当前主网口径：`10086`。

### `witness`
- 类型：object。

```json
{
  "threshold": 1,
  "signatures": [
    {
      "scheme": "ed25519",
      "signature": "0x...",
      "public_key": "0x..."
    }
  ]
}
```

#### `witness.threshold`
- 类型：`u16` / `null`。
- 缺省时节点侧按 `1` 处理。

#### `witness.signatures`
- 当前仅支持 `ed25519`。
- `signature` 必须是 64-byte hex。
- `public_key` 必须是 32-byte hex。

## 三端归一化约定

### CLI
- `rabbitchain compute send` 读取 JSON 后会：
  - 要求顶层必须是 object
  - 自动规范化 `tx_id`
  - 原样提交其余字段
- 当节点拒绝提交时，CLI 会显示 RPC `error.data`，不再只显示 `Invalid params`。

### 浏览器插件钱包
- `Address` owner 支持 `0x...`
- `Ed25519.public_key` 支持单个 `0x...`，并容忍重复前缀输入
- signing preimage 内部编码时再转换成 20-byte address / 32-byte pubkey

### 移动钱包
- 与插件钱包保持同一规则：
  - `Address` owner 使用 `0x...`
  - `Ed25519.public_key` 使用 `0x...`
  - 编码时再转换为底层字节

## 真实可用示例

### `Address` owner 的 `Mint`

```json
{
  "tx_id": "0x...",
  "domain_id": 0,
  "command": "Mint",
  "input_set": [],
  "read_set": [],
  "output_proposals": [
    {
      "output_id": "0x6ec3dc8a0afc5a091844381ab521fec97e4fc80443026627ba649fa45d0691f4",
      "object_id": "0x5acae15f98632b60aab4491d0165e151eb563e79613dab7d20251bf82694a59e",
      "domain_id": 0,
      "kind": "Asset",
      "owner": {
        "type": "Address",
        "address": "0x9aea038CD4255BaaC26eAC5A74e58a07ED2f1975"
      },
      "predecessor": null,
      "version": 1,
      "state": "0x616464726573732d6d696e74",
      "resources": [
        {
          "asset_id": "0xb4209fa39bc8bdf18a711992bc876f508f2ed6f8d21b9a11821ab5b1676c04e0",
          "value": { "type": "Amount", "amount": 9 }
        }
      ],
      "created_at": 0,
      "flags": 0,
      "extensions": []
    }
  ],
  "fee": 0,
  "nonce": 1,
  "metadata": [],
  "payload": "0x",
  "chain_id": 10086,
  "network_id": 10086,
  "witness": { "threshold": 1, "signatures": [] }
}
```

### `Address` owner 的 `Transfer`

```json
{
  "tx_id": "0x...",
  "domain_id": 0,
  "command": "Transfer",
  "input_set": [
    "0x6ec3dc8a0afc5a091844381ab521fec97e4fc80443026627ba649fa45d0691f4"
  ],
  "read_set": [],
  "output_proposals": [
    {
      "output_id": "0x8af4b65581767ddbedaf0bcbb95dc60d02cc17d8e93a2d143cbc063191552424",
      "object_id": "0x5acae15f98632b60aab4491d0165e151eb563e79613dab7d20251bf82694a59e",
      "domain_id": 0,
      "kind": "Asset",
      "owner": {
        "type": "Address",
        "address": "0xA9230F7a17603f07daFD3aD5dbb1dd43Ee34FDAD"
      },
      "predecessor": "0x6ec3dc8a0afc5a091844381ab521fec97e4fc80443026627ba649fa45d0691f4",
      "version": 2,
      "state": "0x616464726573732d7472616e73666572",
      "resources": [
        {
          "asset_id": "0xb4209fa39bc8bdf18a711992bc876f508f2ed6f8d21b9a11821ab5b1676c04e0",
          "value": { "type": "Amount", "amount": 9 }
        }
      ],
      "created_at": 0,
      "flags": 0,
      "extensions": []
    }
  ],
  "fee": 0,
  "nonce": 2,
  "metadata": [],
  "payload": "0x",
  "chain_id": 10086,
  "network_id": 10086,
  "witness": { "threshold": 1, "signatures": [] }
}
```

## 变更规则

以后如果 compute JSON 协议有变更：
- 先更新 `rabbitapi` / `rabbitcore` 实现
- 再更新这份文档
- 再更新插件钱包、移动钱包、CLI 的入口引用

不要再在三端 README 中各自复制一套字段定义。
