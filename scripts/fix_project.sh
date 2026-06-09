#!/bin/bash
# RabbitChain 项目修复脚本
# 用于快速修复编译问题和添加缺失的文件

set -e

echo "🔧 RabbitChain 项目修复脚本"
echo "========================="
echo ""

# 1. 修复 benches 配置问题
echo "📝 修复 benches 配置..."

# 创建基准测试目录
mkdir -p benches

# 创建简单的 trie 基准测试文件
cat > benches/trie_bench.rs << 'EOF'
//! MPT Trie 性能基准测试

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use rabbitstore::trie::{Trie, TrieNode};
use rabbitstore::db::MemoryDB;
use rand::RngCore;

fn generate_random_key() -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut key = vec![0u8; 32];
    rng.fill_bytes(&mut key);
    key
}

fn generate_random_value() -> Vec<u8> {
    let mut rng = rand::thread_rng();
    let mut value = vec![0u8; 256];
    rng.fill_bytes(&mut value);
    value
}

fn bench_trie_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("trie_insert");
    
    for size in [10, 100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            size,
            |b, &size| {
                b.iter(|| {
                    let db = MemoryDB::new();
                    let mut trie = Trie::new(db);
                    
                    for _ in 0..size {
                        let key = generate_random_key();
                        let value = generate_random_value();
                        trie.insert(&key, value).unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

fn bench_trie_get(c: &mut Criterion) {
    let mut group = c.benchmark_group("trie_get");
    
    for size in [100, 1000].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            size,
            |b, &size| {
                b.iter(|| {
                    let db = MemoryDB::new();
                    let mut trie = Trie::new(db);
                    
                    // 预填充数据
                    let mut keys = Vec::new();
                    for _ in 0..size {
                        let key = generate_random_key();
                        let value = generate_random_value();
                        keys.push(key.clone());
                        trie.insert(&key, value).unwrap();
                    }
                    
                    // 基准测试查询
                    for key in &keys {
                        trie.get(key).unwrap();
                    }
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_trie_insert, bench_trie_get);
criterion_main!(benches);
EOF

echo "✅ 创建 benches/trie_bench.rs"

# 2. 创建数据库迁移脚本
echo ""
echo "📝 创建数据库迁移脚本..."

cat > migrations/001_initial_schema.sql << 'EOF'
-- RabbitChain 初始数据库架构
-- 版本：0.1.0
-- 日期：2026-03-05

-- 区块表
CREATE TABLE IF NOT EXISTS blocks (
    hash TEXT PRIMARY KEY,
    number INTEGER NOT NULL UNIQUE,
    parent_hash TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    difficulty TEXT NOT NULL,
    gas_limit INTEGER NOT NULL,
    gas_used INTEGER NOT NULL,
    miner TEXT NOT NULL,
    state_root TEXT NOT NULL,
    transactions_root TEXT NOT NULL,
    receipts_root TEXT NOT NULL,
    extra_data BLOB,
    nonce TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 交易表
CREATE TABLE IF NOT EXISTS transactions (
    hash TEXT PRIMARY KEY,
    block_hash TEXT REFERENCES blocks(hash),
    block_number INTEGER,
    transaction_index INTEGER,
    from_address TEXT NOT NULL,
    to_address TEXT,
    value TEXT NOT NULL,
    gas_price INTEGER NOT NULL,
    gas_limit INTEGER NOT NULL,
    nonce INTEGER NOT NULL,
    input BLOB,
    v INTEGER,
    r TEXT,
    s TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 账户表
CREATE TABLE IF NOT EXISTS accounts (
    address TEXT PRIMARY KEY,
    account_type TEXT NOT NULL,
    balance TEXT NOT NULL,
    nonce INTEGER NOT NULL DEFAULT 0,
    storage_root TEXT,
    code_hash TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 合约代码表
CREATE TABLE IF NOT EXISTS contract_code (
    code_hash TEXT PRIMARY KEY,
    code BLOB NOT NULL,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- UTXO 表
CREATE TABLE IF NOT EXISTS utxos (
    tx_hash TEXT NOT NULL,
    output_index INTEGER NOT NULL,
    address TEXT NOT NULL,
    value TEXT NOT NULL,
    spent BOOLEAN DEFAULT FALSE,
    spent_by TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (tx_hash, output_index)
);

-- 索引
CREATE INDEX IF NOT EXISTS idx_blocks_number ON blocks(number);
CREATE INDEX IF NOT EXISTS idx_blocks_timestamp ON blocks(timestamp);
CREATE INDEX IF NOT EXISTS idx_transactions_block_hash ON transactions(block_hash);
CREATE INDEX IF NOT EXISTS idx_transactions_from ON transactions(from_address);
CREATE INDEX IF NOT EXISTS idx_transactions_to ON transactions(to_address);
CREATE INDEX IF NOT EXISTS idx_utxos_address ON utxos(address);
CREATE INDEX IF NOT EXISTS idx_utxos_spent ON utxos(spent);

-- 元数据表
CREATE TABLE IF NOT EXISTS metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

-- 插入初始元数据
INSERT INTO metadata (key, value) VALUES 
    ('schema_version', '0.1.0'),
    ('chain_id', '1001'),
    ('genesis_hash', ''),
    ('latest_block', '0');
EOF

echo "✅ 创建 migrations/001_initial_schema.sql"

# 3. 创建工具脚本
echo ""
echo "📝 创建工具脚本..."

cat > scripts/devnet.sh << 'EOF'
#!/bin/bash
# RabbitChain 开发网络启动脚本

set -e

echo "🚀 启动 RabbitChain 开发网络"

# 检查构建
if [ ! -f "target/release/rabbitchain" ]; then
    echo "📦 构建项目..."
    cargo build --release
fi

# 创建数据目录
DATA_DIR="${HOME}/.rabbitchain/devnet"
mkdir -p "$DATA_DIR"

# 生成配置文件
cat > "$DATA_DIR/config.toml" << 'TOML'
[network]
port = 30303
bootnodes = []
max_peers = 25

[rpc]
http_enabled = true
http_port = 8545
http_addr = "127.0.0.1"
ws_enabled = true
ws_port = 8546
ws_addr = "127.0.0.1"

[mining]
enabled = true
threads = 2
coinbase = "0x0000000000000000000000000000000000000000"

[logging]
level = "info"
format = "json"
TOML

echo "✅ 配置文件已创建：$DATA_DIR/config.toml"

# 启动节点
echo "🔗 启动节点..."
./target/release/rabbitchain run --config "$DATA_DIR/config.toml" --datadir "$DATA_DIR"
EOF

chmod +x scripts/devnet.sh
echo "✅ 创建 scripts/devnet.sh"

cat > scripts/testnet.sh << 'EOF'
#!/bin/bash
# RabbitChain 测试网启动脚本

set -e

echo "🌐 启动 RabbitChain 测试网"

# 检查构建
if [ ! -f "target/release/rabbitchain" ]; then
    echo "📦 构建项目..."
    cargo build --release
fi

# 创建数据目录
DATA_DIR="${HOME}/.rabbitchain/testnet"
mkdir -p "$DATA_DIR"

# 测试网配置
cat > "$DATA_DIR/config.toml" << 'TOML'
[network]
port = 30303
bootnodes = [
    "enode://testnet-bootnode-1.rabbitchain.io:30303",
    "enode://testnet-bootnode-2.rabbitchain.io:30303"
]
max_peers = 50

[rpc]
http_enabled = true
http_port = 8545
http_addr = "0.0.0.0"
ws_enabled = true
ws_port = 8546
ws_addr = "0.0.0.0"

[mining]
enabled = false

[logging]
level = "info"
format = "json"
TOML

echo "✅ 配置文件已创建：$DATA_DIR/config.toml"

# 启动节点
echo "🔗 启动节点..."
./target/release/rabbitchain run --config "$DATA_DIR/config.toml" --datadir "$DATA_DIR"
EOF

chmod +x scripts/testnet.sh
echo "✅ 创建 scripts/testnet.sh"

cat > scripts/run_tests.sh << 'EOF'
#!/bin/bash
# RabbitChain 测试运行脚本

set -e

echo "🧪 运行 RabbitChain 测试"
echo "======================"
echo ""

# 单元测试
echo "📋 运行单元测试..."
cargo test --lib -- --test-threads=1

echo ""
echo "📋 运行集成测试..."
cargo test --test integration_test

echo ""
echo "📊 测试覆盖率..."
cargo tarpaulin --out Html --output-dir ./coverage

echo ""
echo "✅ 所有测试完成!"
EOF

chmod +x scripts/run_tests.sh
echo "✅ 创建 scripts/run_tests.sh"

cat > scripts/benchmark.sh << 'EOF'
#!/bin/bash
# RabbitChain 性能基准测试脚本

set -e

echo "⚡ 运行 RabbitChain 基准测试"
echo "========================="
echo ""

# 运行基准测试
echo "📊 运行基准测试..."
cargo bench -- --output-format bencher | tee benchmark_results.txt

echo ""
echo "✅ 基准测试完成!"
echo "📁 结果保存在：benchmark_results.txt"
EOF

chmod +x scripts/benchmark.sh
echo "✅ 创建 scripts/benchmark.sh"

# 4. 创建文档
echo ""
echo "📝 创建文档..."

cat > docs/GETTING_STARTED.md << 'EOF'
# RabbitChain 快速入门指南

## 环境要求

- Rust 1.75+
- Cargo
- Linux/macOS/Windows

## 安装

### 从源码构建

```bash
# 克隆仓库
git clone https://github.com/Rabbit-Chain-2026/Rabbit-Chain-node.git
cd Rabbit-Chain-node

# 构建
cargo build --release

# 验证安装
./target/release/rabbitchain --version
```

## 快速启动

### 运行开发节点

```bash
# 使用脚本快速启动
./scripts/devnet.sh

# 或手动启动
./target/release/rabbitchain run --dev
```

### 创建账户

```bash
# 创建新账户
./target/release/rabbitchain account new

# 查看账户列表
./target/release/rabbitchain account list
```

### 发送 Compute 操作

```bash
./target/release/rabbitchain compute send \
  --tx-file ./tx.json
```

## 连接测试网

```bash
./scripts/testnet.sh
```

## RPC 查询

### 获取区块号

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getLatestBlock","id":1}'
```

### 获取余额

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc":"2.0",
    "method":"rabbit_getAccount",
    "params":["0xYourAddress", "latest"],
    "id":1
  }'
```

## 下一步

- 阅读 [架构文档](../ARCHITECTURE.md)
- 查看 [API 文档](../docs/API.md)
- 加入 [Discord 社区](https://discord.gg/rabbitchain)
EOF

echo "✅ 创建 docs/GETTING_STARTED.md"

cat > docs/API.md << 'EOF'
# RabbitChain API 文档

## JSON-RPC 端点

- HTTP: `http://localhost:8545`
- WebSocket: `ws://localhost:8546`

## RabbitChain 信息方法与网络方法

### rabbit_clientVersion

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_clientVersion","id":1}'
```

响应:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": "RabbitChain/v0.1.0/linux/rustc1.75"
}
```

### rabbit_keccak256

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc":"2.0",
    "method":"rabbit_keccak256",
    "params":["0x68656c6c6f20776f726c64"],
    "id":1
  }'
```

### rabbit_*

#### rabbit_getLatestBlock

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"rabbit_getLatestBlock","id":1}'
```

#### rabbit_getAccount

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc":"2.0",
    "method":"rabbit_getAccount",
    "params":["0xAddress", "latest"],
    "id":1
  }'
```

#### rabbit_submitComputeTx

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc":"2.0",
    "method":"rabbit_submitComputeTx",
    "params":["0xSignedTx"],
    "id":1
  }'
```

## RabbitChain 扩展方法

### rabbit_getAccount

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc":"2.0",
    "method":"rabbit_getAccount",
    "params":["0xAddress"],
    "id":1
  }'
```

### rabbit_getUtxos

```bash
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc":"2.0",
    "method":"rabbit_getUtxos",
    "params":["0xAddress"],
    "id":1
  }'
```

## WebSocket 订阅

### 订阅新区块

```javascript
const ws = new WebSocket('ws://localhost:8546');

ws.onopen = () => {
  ws.send(JSON.stringify({
    jsonrpc: '2.0',
    method: 'rabbit_subscribe',
    params: ['newHeads'],
    id: 1
  }));
};

ws.onmessage = (msg) => {
  console.log(JSON.parse(msg.data));
};
```

### 订阅新操作

```javascript
ws.send(JSON.stringify({
  jsonrpc: '2.0',
  method: 'rabbit_subscribe',
  params: ['pendingOperations'],
  id: 2
}));
```

## 错误码

| 错误码 | 描述 |
|--------|------|
| -32700 | 解析错误 |
| -32600 | 无效请求 |
| -32601 | 方法不存在 |
| -32602 | 无效参数 |
| -32000 | 服务器错误 |
EOF

echo "✅ 创建 docs/API.md"

echo ""
echo "🎉 所有文件创建完成!"
echo ""
echo "下一步:"
echo "  1. 运行 'cargo build' 验证编译"
echo "  2. 运行 'cargo test' 运行测试"
echo "  3. 运行 './scripts/devnet.sh' 启动开发网络"
echo ""
