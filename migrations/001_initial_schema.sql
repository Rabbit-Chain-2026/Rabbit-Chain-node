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
