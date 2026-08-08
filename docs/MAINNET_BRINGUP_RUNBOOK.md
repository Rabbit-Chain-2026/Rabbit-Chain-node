# Mainnet Bring-up Runbook

适用目标：

- 受控主网启网
- 受控真实挖矿启动
- 小范围白名单节点 / 白名单矿工 bring-up

不适用目标：

- 一步到位全面公开开放
- 运营成熟度评估

## 1. 角色划分

建议最小拓扑：

1. `bootnode（首节点）`
   - 负责先启动、暴露 RPC、提供 bootnode 入口
   - 可同时承担受控挖矿角色

2. `follower（同步节点）`
   - 仅做节点同步与对外 RPC 观察
   - 初期建议不开挖矿

3. `observer（只读节点）`
   - 只读节点
   - 用于同步一致性与 explorer 读取

4. `pool + miner`
   - 外部挖矿执行面
   - 初期建议先由白名单矿工接入

## 2. Bring-up 前置条件

至少准备：

1. 一个明确的 `coinbase` 地址
2. 至少一个 bootnode 入口（优先 `wss://.../p2p`，必要时可用 `enode://...`）
3. 一台可运行 explorer backend 的机器
4. 一组受控矿池 / 矿工
5. 命令行钱包可创建并可签名

建议先跑：

```bash
cd Rabbit-Chain-node
bash scripts/workspace_acceptance.sh --quick
```

## 3. 启动顺序

### 步骤 1：启动第一个主节点

如果采用节点内置挖矿：

```bash
scripts/mainnet.sh start bootnode --mine --coinbase 0xYOUR_COINBASE
```

如果采用外部矿工模式，建议：

```bash
scripts/mainnet.sh start bootnode \
  --mine \
  --disable-local-miner \
  --coinbase 0xYOUR_COINBASE \
  --rpc-rate-limit-per-minute 0
```

说明：

- `--disable-local-miner` 表示只开放 `rabbit_getWork` / `rabbit_submitWork`，不启动节点内置本地矿工
- `--rpc-rate-limit-per-minute 0` 只建议在受控 bring-up 阶段使用，避免外部矿工 smoke 被默认限流击穿
- 启动后优先从节点日志读取 `bootnode enode hint: ...`，将该值提供给 follower / observer

### 步骤 2：确认首节点可用

检查：

```bash
scripts/mainnet.sh status
scripts/mainnet.sh logs
```

以及：

```bash
curl -sS -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"rabbit_getLatestBlock","params":[],"id":1}' \
  https://rpc.rabbitchain.wedevs.org
```

预期：

- 进程在线
- RPC 可达
- 区块高度增长或至少挖矿 work 可获取

### 步骤 3：启动 follower（同步节点）

用首节点的公网 P2P 入口作为 `--bootnode`：

```bash
scripts/mainnet.sh start follower \
  --bootnode wss://wss.rabbitchain.wedevs.org/p2p
```

说明：

- follower 初期建议不开挖矿
- 如果需要直连 TCP bootnode，可把 `--bootnode` 改成 `enode://peer@ip:port`
- 当前公网入口优先使用 `wss://wss.rabbitchain.wedevs.org/p2p`

### 步骤 4：启动 observer（只读节点）

observer 建议与 follower 类似，但不承担外部流量入口：

```bash
scripts/mainnet.sh start observer \
  --bootnode wss://wss.rabbitchain.wedevs.org/p2p
```

说明：

- observer 的 `--bootnode` 口径与 follower 相同，优先使用 `wss://wss.rabbitchain.wedevs.org/p2p`
- 如果需要直连 TCP bootnode，可改成 `enode://peer@ip:port`
- 如果 bootnode 要放在 Cloudflare CDN 后面，可使用 `wss://...` WebSocket P2P bootnode，并关闭 discovery；见 [P2P 传输](book/40-network/p2p-transport.md)。

### 步骤 5：启动矿池与矿工

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

如果是受控 bring-up，想先降低本地验收难度，可暂时在节点侧显式设置：

```bash
scripts/mainnet.sh start bootnode \
  --mine \
  --disable-local-miner \
  --coinbase 0xYOUR_COINBASE \
  --rpc-rate-limit-per-minute 0
```

正式公开阶段不建议长期保留这种 bring-up 参数。

### 步骤 6：启动 explorer

后端：

```bash
cd ../rabbitchain-explorer/backend
RABBIT_RPC_URL=https://rpc.rabbitchain.wedevs.org cargo run --release
```

## 4. 核心观察项

### A. 命令行钱包

至少确认：

```bash
rabbitchain wallet new --name bringup-wallet --scheme ed25519 --passphrase 'StrongPassphrase123!'
rabbitchain wallet list
rabbitchain wallet sign --name bringup-wallet --message hello --passphrase 'StrongPassphrase123!'
```

### B. 挖矿

至少确认：

- `rabbit_getWork` 可返回 job
- `rabbit_submitWork` 可返回 accepted
- pool `/v1/stats` 中 shares 增长
- miner `/metrics` 中 accepted/hash counters 增长

### C. 同步

建议执行：

```bash
scripts/node_sync_check.sh
scripts/mainnet_checklist.sh
```

至少确认：

- `net_version` 一致
- `net_peerCount >= 1`
- 区块高度差在阈值内
- `rabbit_peers` 能看到对端

## 5. 回退动作

如果 bring-up 过程中出现异常：

1. 先停止矿工
2. 再停止矿池
3. 保留 bootnode
4. 检查 follower / observer 是否仍能同步
5. 如需整体回退，再按节点顺序停止

节点停止：

```bash
scripts/mainnet.sh stop bootnode
scripts/mainnet.sh stop follower
scripts/mainnet.sh stop observer
```

查看日志：

```bash
scripts/mainnet.sh logs bootnode
scripts/mainnet.sh logs follower
scripts/mainnet.sh logs observer
```

## 6. 当前推荐口径

当前推荐做法不是“全面公开上线”，而是：

1. 先 1 个 `bootnode`（首节点）
2. 再 1 个 `follower`（同步节点）
3. 再 1 个 `observer`（只读节点）
4. 再接 1 组白名单 pool/miner
5. 连续观察同步、出块、shares、钱包创建
6. 稳定后再逐步扩容

## 7. 直接命令清单

如果需要直接照抄执行，相关命令口径已并入 [主网拉起](book/70-operations/mainnet-bringup.md)，并由 `scripts/mainnet.sh` 与 `scripts/mainnet_local_bringup.sh` 承接。
