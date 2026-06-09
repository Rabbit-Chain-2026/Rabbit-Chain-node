# Real Sync Remediation（2026-03-08）

目标：修复 follower 仅同步头部、不同步账户状态与交易索引的问题，按问题清单逐项闭环。

## 1. 协议层未传真实交易/状态数据
- 现状（修复前）：`SyncBlockBody` 仅 `block_hash + tx_count`，`SyncStateSnapshot` 仅摘要。
- 修复：
  - `SyncStateSnapshot` 增加 `accounts`、`compute_txs`。
  - 新增同步记录类型：`SyncComputeTxRecord`。
  - 控制帧增加 `*_V2` 编码：`RABBIT/BLOCK_BODY_V2`、`RABBIT/STATE_SNAPSHOT_V2`，负载为 JSON(hex)。
- 代码：
  - `crates/rabbitnet/src/protocol.rs`
  - `crates/rabbitnet/src/lib.rs`

## 2. 同步执行层丢弃 body，仅写空交易块
- 现状（修复前）：`Ok(_body) => {}`，随后写入空块体。
- 修复：
  - body 阶段缓存 `SyncBlockBody`，并在 compute-only 模式下要求 `tx_count == 0`。
  - 迁移期间的 legacy 兼容路径只保留 header 语义，不再承载旧 transaction 列表。
  - 对 legacy 块保留空根兼容；新生产块则由 body 计算 `transactions_root`。
- 代码：
  - `crates/rabbitnet/src/sync.rs`

## 3. 状态快照是合成摘要，不来自真实状态
- 现状（修复前）：`derive_state_root/proof` 仅基于 block hash 合成。
- 修复：
  - 快照由真实同步缓存导出：账户、compute 索引。
  - `state_root` 改为对账户快照确定性哈希（按地址排序）。
  - `state_proof` 改为绑定 `header.hash + snapshot 内容` 的摘要。
  - follower 验证通过后落地替换本地同步缓存。
  - 增加“同高度周期性状态刷新”：即使高度已追平，也继续拉最新快照，避免高度不变时状态漂移。
- 代码：
  - `crates/rabbitnet/src/sync.rs`
  - `crates/rabbitnet/src/lib.rs`（全局同步缓存）

## 4. RPC 只读本地状态/本地索引
- 现状（修复前）：`rabbit_getAccount`、`rabbit_getOperationByHash` 不看同步缓存。
- 修复：
  - `rabbit_getAccount`：本地账户缺失时回退读取同步账户快照。
  - `rabbit_getOperationByHash`：本地未命中时回退读取同步 compute 索引。
  - `rabbit_listOperations`、`rabbit_listComputeTxResults` 合并本地 + 同步索引并去重。
  - 本地状态变更时把账户/索引写入全局同步缓存（compute、block reward）。
- 代码：
  - `crates/rabbitapi/src/rpc/mod.rs`

## 5. `rabbit_importBlock` 导入逻辑
- 现状（修复前）：导入时强制写空块体。
- 修复：
  - compute-only 模式下显式拒绝 legacy `transactions` 参数。
  - 导入块时对 legacy 兼容块保留 header 语义；对 body-bearing 块则校验并写入真实 `transactions_root`。
- 代码：
  - `crates/rabbitapi/src/rpc/mod.rs`

## 检查与验证
- 单元/集成测试：
  - `cargo test -p rabbitnet -p rabbitapi` 通过。
  - 新增测试：`test_rabbit_import_block_with_transactions_updates_tx_index`。
- 本地双节点联调（A 挖矿 + B follower）：
  - 高度同步后验证区块高度、矿工状态与 compute 结果查询保持一致。
- 检查脚本增强：
  - `scripts/mainnet_checklist.sh` 新增 local vs remote 账户一致性检查；可选 `CHECK_TX_HASH` 交易索引一致性检查。

## 仍需后续推进（主网化）
- 当前 transfer 仍是 RPC 直改状态的 MVP 语义；主网化应转为“交易入池 -> 出块执行 -> 状态转移”单一路径。
- 快照/状态证明目前为确定性摘要，不是 MPT 证明；后续应升级到可验证状态证明与增量状态同步。
