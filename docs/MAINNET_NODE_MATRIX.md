# Mainnet Node Matrix

第一批主网 bring-up 建议采用 3 节点 + 1 组矿池/矿工：

## 节点矩阵

| 角色 | 职责 | 默认 HTTP RPC | 默认 WS | 默认 P2P | 是否挖矿 | 是否开放 work RPC | 是否对外提供读 RPC |
|---|---|---:|---:|---:|---|---|---|
| `bootnode` | 首节点 | `https://rpc.rabbitchain.wedevs.org` | `wss://wss.rabbitchain.wedevs.org/p2p` | `30303` | 是 | 是 | 可选 |
| `follower` | 同步节点 / 公网 RPC 候选 | `29645` | `29646` | `31303` | 否 | 否 | 是 |
| `observer` | 只读节点 / explorer 数据源 | `39745` | `39746` | `32303` | 否 | 否 | 是 |

## 推荐配置

### 1. bootnode（首节点）

适用：

- 第一个主节点
- 受控真实挖矿入口
- pool 对接的首选节点

推荐命令：

```bash
scripts/mainnet.sh start bootnode \
  --mine \
  --disable-local-miner \
  --coinbase 0xYOUR_COINBASE \
  --rpc-rate-limit-per-minute 0
```

说明：

- `--disable-local-miner`：外部矿工模式推荐打开
- `--rpc-rate-limit-per-minute 0`：只建议 bring-up 阶段使用

### 2. follower（同步节点）

适用：

- 公网同步节点
- 对外公共 RPC 候选

推荐命令：

```bash
scripts/mainnet.sh start follower \
  --bootnode wss://wss.rabbitchain.wedevs.org/p2p
```

说明：

- 初期不开挖矿
- 初期不开放 `rabbit_getWork` / `rabbit_submitWork`
- `--bootnode` 当前优先使用 `wss://wss.rabbitchain.wedevs.org/p2p`
- 如果需要直连 TCP bootnode，可改成 `enode://peer@ip:port`

### 3. observer（只读节点）

适用：

- explorer backend 数据源
- 同步一致性核验
- 只读观测

推荐命令：

```bash
scripts/mainnet.sh start observer \
  --bootnode wss://wss.rabbitchain.wedevs.org/p2p
```

说明：

- 不承担挖矿
- 不承担 pool 入口
- `--bootnode` 当前优先使用 `wss://wss.rabbitchain.wedevs.org/p2p`
- 如果需要直连 TCP bootnode，可改成 `enode://peer@ip:port`

## 挖矿执行面矩阵

| 组件 | 推荐连接对象 | 默认端口 | 说明 |
|---|---|---:|---|
| `rabbitchain-mining-stack pool` | `bootnode` RPC | `9332` | 只连一个受控节点即可开始 bring-up |
| `rabbitchain-mining-stack miner` | `pool` | `9333` metrics | 初期建议白名单矿工 |

矿池：

```bash
cd ../rabbitchain-mining-stack
cargo run --release -- \
  pool \
  --host 0.0.0.0 \
  --port 9332 \
  --node-rpc https://rpc.rabbitchain.wedevs.org
```

矿工：

```bash
cargo run --release -- \
  miner \
  --pool-url http://POOL_IP:9332 \
  --miner-id miner-mainnet-1
```

## explorer 矩阵

| 组件 | 推荐连接对象 | 默认端口 | 说明 |
|---|---|---:|---|
| `rabbitchain-explorer backend` | `observer` 或 `bootnode` RPC | `18080` | 优先接 observer |
| `rabbitchain-explorer frontend` | backend | Vite preview 自定 | 生产时由 backend/静态托管统一承接 |

推荐：

- bring-up 初期，优先让 explorer backend 连 `observer`
- 若 observer 尚未就绪，再临时连 `bootnode`

## bring-up 阶段建议

### 阶段 1

- `bootnode` 1 台
- `pool` 1 套
- `miner` 1 套

目标：

- 先确认出块和 accepted share

### 阶段 2

- 增加 `follower` 1 台

目标：

- 确认 P2P 同步与高度收敛

### 阶段 3

- 增加 `observer` 1 台
- explorer backend 接 observer

目标：

- 确认只读观测面稳定

### 阶段 4

- 增加更多 follower
- 扩大白名单矿工范围

目标：

- 扩容前先验证同步、稳定性与可观测性
