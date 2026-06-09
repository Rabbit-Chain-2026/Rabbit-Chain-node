# RabbitChain 快速入门指南

## 环境要求

- Rust 1.75+
- Cargo
- Linux/macOS/Windows

## 安装

```bash
git clone https://github.com/Rabbit-Chain-2026/Rabbit-Chain-node.git
cd Rabbit-Chain-node
cargo build --release
./target/release/rabbitchain --version
```

## 快速启动

```bash
# 初始化数据目录
./target/release/rabbitchain --network local init

# 启动本地节点
./target/release/rabbitchain --network local run
```

## 创建账户

```bash
./target/release/rabbitchain account new \
  --name ed25519-1 \
  --scheme ed25519

./target/release/rabbitchain account list
```

如果要创建命令行钱包作为挖矿 coinbase，并启动内置矿工或外部 pool/miner，请直接按完整教程执行：

- [CLI_WALLET_MINING_TUTORIAL.md](CLI_WALLET_MINING_TUTORIAL.md)

## 提交 Compute 操作

```bash
./target/release/rabbitchain compute send \
  --tx-file ./tx.json

./target/release/rabbitchain compute get --tx-id 0x...
```

## RPC 查询

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getAccount","params":["0xYourAddress"],"id":1}'
```

## 下一步

- 阅读 [架构文档](../ARCHITECTURE.md)
- 查看 [API 文档](../docs/API.md)
- 加入 [Discord 社区](https://discord.gg/rabbitchain)
