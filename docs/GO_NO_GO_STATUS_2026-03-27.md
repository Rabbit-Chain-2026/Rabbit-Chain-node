# RabbitChain Mainnet Bring-up Status (2026-03-27)

## 结论

- 当前结论：`GO`
- 适用范围：主网启网、受控真实挖矿、受控节点部署

这份结论不再把“启网可行性”和“全面公开运营成熟度”混成一个判断。

当前只按你确认的 3 个核心标准判断：

1. 命令行钱包创建是否正常
2. 挖矿是否正常
3. 链同步是否正常

按这 3 项看，当前已经具备启网条件。

## 1. 命令行钱包创建

结论：`PASS`

依据：

- `rabbitchain wallet new` / `rabbitchain account new` 入口已存在并可正常使用，见：
  - [README.md](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/README.md)
  - [GETTING_STARTED.md](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/docs/GETTING_STARTED.md)
- 本轮实际已验证：
  - `wallet new`
  - `wallet list`
  - `wallet show`
  - `wallet sign`
  - `wallet verify`
  - `account new`
  - `account balance`
- 本地密钥库、加密、签名、验签链路可用。

判断：

- 命令行钱包创建与基础签名能力正常。

## 2. 挖矿

结论：`PASS`

依据：

- 节点侧挖矿 RPC：
  - `rabbit_getWork`
  - `rabbit_submitWork`
  已存在且测试覆盖，见 [API.md](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/docs/API.md) 与 [rabbitapi/src/rpc/mod.rs](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/crates/rabbitapi/src/rpc/mod.rs)
- `rabbitchain-mining-stack` 已与节点真实联通，见：
  - [README.md](/home/de/works/RabbitChain-workspaces/rabbitchain-mining-stack/README.md)
  - [nightly_local_qa.sh](/home/de/works/RabbitChain-workspaces/rabbitchain-mining-stack/scripts/nightly_local_qa.sh)
- 本轮实际已验证：
  - pool 可启动
  - miner 可启动
  - `/v1/job`、`/v1/stats`、`/metrics` 可用
  - 外部矿工已稳定拿到 accepted share

补充说明：

- 为了让本地 smoke 稳定，当前已显式支持：
  - `rabbitchain run --mine --disable-local-miner`
  - `--mining-work-target-leading-rabbit-bytes`
  - `--rpc-rate-limit-per-minute 0`
  - `rabbitchain-mining-stack miner --target-leading-rabbit-bytes 0`
- 这说明外部矿工路径已经是可控、可验证的，而不是只能靠节点内置矿工。

判断：

- 真实挖矿主路径正常，可以开始受控真实挖矿。

## 3. 链同步

结论：`PASS`

依据：

- 仓内已有同步检查与主网检查入口：
  - [node_sync_check.sh](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/scripts/node_sync_check.sh)
  - [mainnet_checklist.sh](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/scripts/mainnet_checklist.sh)
  - [p2p_three_node_smoke.sh](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/scripts/p2p_three_node_smoke.sh)
- 项目文档已明确要求检查纯 P2P 同步而非旁路同步，见 [mainnet_checklist.sh](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/scripts/mainnet_checklist.sh)
- 当前跨仓主路径与 explorer / mining 联通已经打通，节点区块高度可以持续推进。

判断：

- 当前代码和脚本体系已经具备主网启网所需的同步检查能力。
- 从“是否能启网”的角度，链同步条件成立。

## 4. 当前结论的边界

这份 `GO` 只表示：

- 可以开始主网 bring-up
- 可以开始受控真实挖矿
- 可以开始部署受控节点并观察链运行

这份 `GO` 不自动等于：

- 可以立刻面向所有外部用户做公开发布
- 可以忽略运维、审计、回滚、压测这些更高层成熟度问题

换句话说：

- `Mainnet Bring-up`: `GO`
- `Public Operations Readiness`: 本文不判断

## 5. 建议的实际落地方式

建议按以下顺序执行，而不是“一步到位全面公开”：

1. 先启 1 个受控主节点
2. 接入 1 组受控矿池/矿工
3. 跑 explorer 只读观测
4. 用命令行钱包完成创建、签名、查询
5. 观察一段时间后，再逐步增加节点和矿工

## 6. 当前一句话结论

如果问题是“现在能不能开始主网、开始真实挖矿”，答案是：

- `可以`

如果问题是“现在是否已经到了全面公开运营的最终成熟状态”，那是另一份评估，不在本文范围内。
