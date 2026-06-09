# PoW Target / Sync Debug Record（2026-05-31）

目标：记录 `proof-of-work` 从“前导零字节计数”切换到“完整 256-bit target 比较”后，我们在真实节点、矿池/矿工联调、P2P 自动跟随中遇到的问题、排查过程、修复和最终验证结论。

相关提交：

- `4f31068` `fix(pow): switch mining to full target validation`
- `085c0fd` `fix(mining): consume full node pow targets`
- `8a21c2d` `fix(sync): include header version in p2p sync`

## 1. 背景

修复前，链上 PoW 难度只按“前导零字节数”判断：

- `2` 字节前导零：大约 `2^16`
- `3` 字节前导零：大约 `2^24`
- `4` 字节前导零：大约 `2^32`

这会带来两个直接问题：

- 难度调节是按 `256x` 的台阶跳变，不是连续可调。
- 目标出块时间即使设为 `10s`，实际也只能在几个离散档位之间震荡。

在局域网单机挖矿场景下，我们看到的现象就是：

- `3` 字节目标时太快
- `4` 字节目标时太慢
- 最近区块间隔呈现“很久出一块，然后连续很快几块”的抖动

因此决定切换到完整 target 规则：

```text
digest_as_u256 <= target
target = max_target / difficulty
```

## 2. 问题一：PoW 规则切换后，需要先修底层整数与全链路一致性

### 2.1 现象

在把 `rabbit_getWork / rabbit_submitWork / rabbit_importBlock / rabbitnet sync` 改成完整 target 比较后，代码层面虽然能编译，但测试和联调暴露出几个底层问题：

- 目标值异常，`target_leading_rabbit_bytes` 退化成 `32`
- target 计算结果不连续，明显不合理
- 真实 share 被节点拒绝，提示 `pow_below_target`

### 2.2 排查过程

先确认了当前实现确实已经改到完整 target：

- `rabbitapi` 返回 `target`
- `rabbitcli` 本地 miner 按 `target` 求解
- `rabbitchain-mining-stack` pool/miner 优先消费 `target`
- `rabbitnet` 校验 `version=2` 区块时使用 `digest <= target`

然后逐步定位到两类底层问题：

1. `U256` 除法内部使用了不可靠路径
2. `max_target / difficulty` 不能继续依赖通用 `U256 / U256`

具体症状包括：

- `pow_target_from_difficulty(1_000_000)` 的结果异常
- 测试里 target 退化成零或极小值
- 节点实际拒绝 miner 提交的 share

### 2.3 根因

根因有两个：

1. [crates/rabbitcore/src/account/account.rs](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/crates/rabbitcore/src/account/account.rs) 里的 `U256` 除法实现存在位序/内部运算问题。
2. `pow_target_from_difficulty()` 走通用 `U256 / U256` 不适合当前这条共识路径；而这里的 `difficulty` 实际上是小整数，完全可以走确定性的专用长除法。

### 2.4 修复

做了两类修复：

- 修正 `U256` 除法中的位设置和中间步骤。
- 在 [crates/rabbitcore/src/block/mod.rs](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/crates/rabbitcore/src/block/mod.rs) 单独实现 `max_target / u128 difficulty` 的 PoW target 计算，不再依赖通用 `U256 / U256`。

同时统一了全链路：

- `rabbitapi`：`rabbit_getWork` 返回完整 `target`
- `rabbit_submitWork`：按完整 target 验证 share
- `rabbit_importBlock`：按完整 target 验证 `version=2` 区块
- `rabbitnet`：同步校验 `version=2` 区块按完整 target，旧 `version=1` 继续兼容旧规则
- `rabbitcli`：本地矿工按完整 target 试 nonce
- `rabbitchain-mining-stack`：pool/miner 优先使用完整 target，旧 `target_leading_rabbit_bytes` 只保留兼容 fallback

### 2.5 验证

通过的检查：

- `cargo test -p rabbitcore pow_target`
- `cargo test -p rabbitapi rabbit_get_work`
- `cargo test -p rabbitapi rabbit_submit_work`
- `cargo test -p rabbitnet`
- `cargo check -p rabbitcli`
- `cd ../rabbitchain-mining-stack && cargo test && cargo check`

## 3. 问题二：CLI 挖矿 smoke 失败，miner 提交的 share 被节点最终拒绝

### 3.1 现象

`bash scripts/cli_mining_smoke.sh` 首次失败，现象是：

- miner 启动
- pool 能取到 `rabbit_getWork`
- `rabbit_submitWork` 返回内部错误
- 高度始终不增长

pool 日志里反复出现：

```text
failed to persist mined block: Protocol error: pow_below_target
```

### 3.2 排查过程

脚本本身带了一个开发加速参数：

```bash
--mining-work-target-leading-rabbit-bytes 0
```

这个参数在旧规则下表示“非常容易命中的工作模板”，方便本地 smoke。

改成 full-target 后，最初实现只在 `rabbit_getWork / rabbit_submitWork` 的工作模板和 share 验证路径上尊重这个覆盖项，但真正写入全局区块缓存时，仍走标准共识校验。

结果就是：

1. miner 能拿到一个很宽松的 work target
2. share 在 RPC 层先通过
3. 真正落块时又被全局共识校验按正常 target 打回

### 3.3 根因

开发覆盖项只影响了“工作分发和 share 接受”，没有影响“dev smoke 下本地块是否允许进入本地历史”。

### 3.4 修复

在 [crates/rabbitapi/src/rpc/mod.rs](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/crates/rabbitapi/src/rpc/mod.rs) 里把这个覆盖项明确定义为：

- 仅在显式传入 `--mining-work-target-leading-rabbit-bytes` 时生效
- 只用于开发/烟测节点
- 这种模式下本地块不进入全局已验证同步缓存

这样做的结果：

- smoke / bring-up 可以快速出块
- 默认节点和主网节点不受影响
- 不会再出现“share 先接收、落块时再打回”的自相矛盾行为

### 3.5 验证

重新执行：

```bash
bash scripts/cli_mining_smoke.sh
```

结果通过，`cli + mining smoke passed`。

## 4. 问题三：本机保护节点不再自动跟随局域网主挖矿节点

### 4.1 现象

部署新版后，局域网节点已经开始产出 `version=2` 区块，但本机保护节点卡在旧高度，例如：

- 本机保护节点：`0x4c`
- 局域网节点：`0x70` 甚至更高

而且本机日志持续出现：

```text
sync invalid headers from peer ... start 77: first_header_parent_or_pow_invalid
```

这说明：

- P2P 连接是正常的
- 同步请求也在正常发
- 失败发生在“收到第一个 header 后的校验”

### 4.2 排查过程

先检查本机保护节点：

- `rabbit_peers` 有远端 peer
- `rabbit_syncStatus` 显示不再增长
- 日志固定失败在 `start 77`

然后对比本地 `76` 和远端 `77` 的持久化记录，重点看：

- `parent_hash`
- `difficulty`
- `mix_hash`
- `header_version`

最终发现远端持久化记录里：

- `76` 还是旧 `header_version=1`
- `77` 开始已经是 `header_version=2`

但 P2P 同步协议里的 `SyncHeader` 结构根本没有 `version` 字段。于是远端发出的 `version=2` 区块头，到了本机 follower 侧会被还原成：

- `version=1`

接着 follower 用 `version=1` 的旧规则去重建 header hash / PoW 语义，第一块就失败，表现为：

```text
first_header_parent_or_pow_invalid
```

### 4.3 根因

协议层漏传了 `header.version`。

受影响路径：

- [crates/rabbitnet/src/protocol.rs](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/crates/rabbitnet/src/protocol.rs)
- [crates/rabbitnet/src/sync.rs](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/crates/rabbitnet/src/sync.rs)
- [crates/rabbitnet/src/lib.rs](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/crates/rabbitnet/src/lib.rs)

### 4.4 修复

修复点：

1. `SyncHeader` 增加 `version`
2. `sync_header_from_block()` 发送时写入 `block.header.version`
3. `block_from_sync_header()` 恢复时使用 `header.version`
4. `RABBIT/HEADERS` 解析兼容两种格式：
   - 旧 `9` 字段格式：默认 `version=1`
   - 新 `10` 字段格式：显式带 `version`

对应提交：

- `8a21c2d` `fix(sync): include header version in p2p sync`

### 4.5 验证

验证顺序：

1. 重新编译 `rabbitchain` release
2. 本机保护节点切到新二进制
3. 局域网挖矿节点切到新二进制
4. 重新观察本机 follower 是否自动追平

结果：

- 本机保护节点从自己的持久化头 `0x54` 自动追到远端 `0xa2`
- 后续继续跟到 `0xa3`
- `rabbit_syncStatus` 恢复为随远端增长
- 本机日志中不再出现 `first_header_parent_or_pow_invalid`

## 5. 最终状态

截至 `2026-05-31` 本轮修复完成后：

- 新挖出的块为 `version=2`
- PoW 使用完整 `256-bit target` 比较
- 矿池/矿工闭环联调通过
- 本机保护节点恢复自动 P2P 跟随
- 旧 `version=1` 历史块仍可继续读取和校验

实测在局域网单机挖矿下，最近一段 `version=2` 区块间隔已经从旧规则下的“几百秒/几千秒级抖动”回到可调区间。一次采样中最近 `10` 个 `version=2` 区块平均约 `7.5s/块`，已经接近 `10s` 目标。

## 6. 经验总结

这次问题链条说明了三个工程原则：

1. 共识规则改动不能只改 RPC 和 miner，必须同时改：
   - 共识校验
   - P2P 同步头协议
   - 本地持久化恢复
   - 烟测 / 开发覆盖项

2. 一旦引入 `header version` 分叉语义，协议和持久化层必须显式携带该字段，不能靠默认值猜。

3. 开发便捷参数和主网语义必须严格隔离。`--mining-work-target-leading-rabbit-bytes` 这类覆盖项只能留在显式 dev/test 路径，不能污染默认共识面。

## 7. 建议后续动作

- 把 `rabbit_getLatestBlock` / explorer / 相关前端也统一展示 `version`
- 增加一个专门的双节点回归脚本：
  - 旧 `version=1` 历史头
  - 新 `version=2` 实时追块
  - follower 重启后自动续追
- 为 `SyncHeader` 增加显式协议版本注释，避免后续再次遗漏字段
