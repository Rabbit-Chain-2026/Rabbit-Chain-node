# Cross-Stack Completeness Report (2026-03-08)

范围：`Rabbit-Chain-node`、OpenTelemetry、`rabbitchain-mining-stack`、`rabbitchain-explorer` 后端。

## 横向对比

| 子系统 | 当前完成度 | 已完成能力 | 未完整项 / 缺口 |
|---|---|---|---|
| 链核心（Rabbit-Chain-node RPC） | 中高 | 账户、对象、domain、compute 提交/查询、挖矿工作流、最新区块、指标、peers | 历史区块检索在本次前缺失；本次已补 `rabbit_getBlockByNumber` / `rabbit_getBlocksRange`，但仍无“全历史强一致索引 + 交易内明细索引” |
| OpenTelemetry | 中 | `rabbitchain` 与 `rabbitchain-mining-stack` 支持 OTLP 开关与导出；Prometheus 指标可用 | `rabbitchain-explorer` 后端尚未接入 OTLP；跨组件 trace_id 关联未闭环；告警策略与SLO落地仍是流程项 |
| 矿池（rabbitchain-mining-stack） | 中 | pool/miner 闭环、share 校验、指标、基础可观测性 | README 明确仍是 MVP：缺少收益结算、难度分账、Stratum 兼容入口、持久化账务 |
| 区块浏览器后端（rabbitchain-explorer/backend） | 中高（本次提升） | 网络/区块/账户/对象/搜索、缓存、活动追踪 | 之前只能依赖 latest block；本次已升级历史块与矿工/交易聚合接口，但仍缺 token 转账、合约事件、内部交易、标签体系等 Etherscan 全功能 |

## 本次新增（已落地）

1. `Rabbit-Chain-node` 新增 RPC：
   - `rabbit_getBlockByNumber`
   - `rabbit_getBlocksRange`
   - `rabbit_listComputeTxResults`
2. `Rabbit-Chain-node` 出块历史索引：
   - mined/imported 区块写入历史缓存，供浏览器查询窗口使用。
3. `rabbitchain-explorer` 后端增强：
   - `/api/overview`
   - `/api/miners`
   - `/api/miners/:address`
   - `/api/accounts/:address/blocks`
   - `/api/txs/recent`
   - `/api/blocks*` 全面切换到链上历史区块 RPC。

## 验证

- `Rabbit-Chain-node`: `cargo test -p rabbitapi` 全通过。
- `rabbitchain-explorer/backend`: `cargo check` 通过。
- `rabbitchain-explorer/frontend`: `npm run build` 通过。
