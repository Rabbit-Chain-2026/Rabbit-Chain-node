# Mainnet First-Wave Commands

这份文档提供第一批 bring-up 的可直接执行命令。

## 0. 先替换这些变量

最少需要确认并替换：

- `COINBASE`
- `REMOTE_HOST`
- `REMOTE_P2P_PORT`
- `POOL_IP`
- `BOOTNODE_ENODE`

适用前提：

- `bootnode`：当前机器或首个受控主节点
- `follower`：本地公网 follower
- `observer`：本地 observer
- `remote public node`：当前既有远端主机口径 `139.180.207.66`

若 IP / 机器不同，只替换文中的地址与 coinbase。

## 1. 固定变量

```bash
export COINBASE="0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9"
export REMOTE_HOST="139.180.207.66"
export REMOTE_P2P_PORT="30303"
export BOOTNODE_ENODE="enode://bootnode-1@${REMOTE_HOST}:${REMOTE_P2P_PORT}"
```

注意：

- 这里的 `BOOTNODE_ENODE` 应使用 `enode://peer@ip:port`
- 不要直接把节点日志里的 `discovery local ENR:` 字符串原样传给 `--bootnode`
- 启动 `bootnode` 后，优先使用日志里打印出来的 `bootnode enode hint: ...`

## 2. 启动 bootnode

如果当前机器承担首个主节点，并且要给外部矿池/矿工提供 work：

```bash
cd Rabbit-Chain-node
./scripts/mainnet.sh start bootnode \
  --mine \
  --disable-local-miner \
  --coinbase "${COINBASE}" \
  --rpc-rate-limit-per-minute 0
```

查看状态：

```bash
./scripts/mainnet.sh status bootnode
./scripts/mainnet.sh logs bootnode
```

## 3. 启动 follower

```bash
cd Rabbit-Chain-node
./scripts/mainnet.sh start follower \
  --bootnode "${BOOTNODE_ENODE}"
```

查看状态：

```bash
./scripts/mainnet.sh status follower
./scripts/mainnet.sh logs follower
```

## 4. 启动 observer

```bash
cd Rabbit-Chain-node
./scripts/mainnet.sh start observer \
  --bootnode "${BOOTNODE_ENODE}"
```

查看状态：

```bash
./scripts/mainnet.sh status observer
./scripts/mainnet.sh logs observer
```

## 5. 启动矿池

```bash
cd rabbitchain-mining-stack
cargo run --release -- \
  pool \
  --host 0.0.0.0 \
  --port 9332 \
  --node-rpc "http://${REMOTE_HOST}:8545"
```

## 6. 启动矿工

```bash
cd rabbitchain-mining-stack
cargo run --release -- \
  miner \
  --pool-url "http://${REMOTE_HOST}:9332" \
  --miner-id miner-mainnet-1
```

## 7. 启动 explorer backend

推荐先接 observer：

```bash
cd rabbitchain-explorer/backend
RABBIT_RPC_URL="http://127.0.0.1:39745" cargo run --release
```

如果 observer 尚未就绪，可临时接 bootnode：

```bash
cd rabbitchain-explorer/backend
RABBIT_RPC_URL="http://${REMOTE_HOST}:8545" cargo run --release
```

## 8. 命令行钱包验收

```bash
cd Rabbit-Chain-node
./target/release/rabbitchain wallet new --name bringup-wallet --scheme ed25519 --passphrase 'StrongPassphrase123!'
./target/release/rabbitchain wallet list
./target/release/rabbitchain wallet sign --name bringup-wallet --message hello --passphrase 'StrongPassphrase123!'
```

## 9. 同步检查

```bash
cd Rabbit-Chain-node
scripts/node_sync_check.sh
scripts/mainnet_checklist.sh
```

## 10. 停止命令

```bash
cd Rabbit-Chain-node
./scripts/mainnet.sh stop observer
./scripts/mainnet.sh stop follower
./scripts/mainnet.sh stop bootnode
```
