# 审计发现（2026-06-15）

按风险从高到低列出，附文件:行号与建议修复方向。

---

## 🔴 高 — 静默回退（违反 AGENTS.md / 工程红线）

### F1. P2P `listen_addr` 解析失败时静默监听 `0.0.0.0`
- 文件：`Rabbit-Chain-node/crates/rabbitnet/src/discovery.rs:151-155`
- 代码：
  ```rust
  let listen_ip = self.config.listen_addr
      .parse::<IpAddr>()
      .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
  ```
- 问题：用户配置 `listen_addr = 10.0.0.5` 但拼错为 `10.0.0..5`，节点会**默默监听所有接口**而不是报错。用户以为只在内网可连，实际上公网也能连，**安全 + 运维双重风险**。
- 修复：`parse` 后用 `?` 把 `AddrParseError` 透出为 `NetworkError`，禁止 `unwrap_or(UNSPECIFIED)`。建议同时在 `no_silent_fallback.sh` 里加一条 `\.parse\(\)\.unwrap_or\(IpAddr` 模式。

### F2. Chrome 钱包 vault 解密错误统一返回 `null`，与 README fail-fast 声明矛盾
- 文件：`rabbitchain-wallet-chrome/src/core/wallet/KeyStore.ts:113-139`
- 代码：
  ```ts
  } catch {
    return null
  }
  ```
- 问题：README 写「默认 fail-fast：vault 解密失败、解析失败、账户条目字段缺失或私钥非法时会直接报错」。但底层 `decryptVault` 把 **所有失败**（密码错、损坏、版本不支持、解 tag 失败、JSON 解析失败）都吞成 `null`，错误信息变成误导性的「Invalid password or wallet not found」。`WalletService.unlock/loadExistingVaultForUpdate` 确实向上 throw，但语义已被吞掉一层。
- 修复：把失败原因以枚举（如 `VaultErrorKind::{WrongPassword, Corrupted, UnsupportedVersion, MalformedJson}`）通过异常抛出，`unlock` 时分别给用户提示。

### F3. Replay 防护注册表仅在内存中，节点重启即失效
- 文件：`Rabbit-Chain-node/crates/rabbitcore/src/compute/execution.rs:497, 510, 533`
- 问题：`BasicTxExecutor::replay_nonce_registry: RwLock<HashMap>` 是 per-executor 实例；节点重启 / 切块后清空。攻击者可在窗口期（`REPLAY_NONCE_WINDOW_SECS = 3600`）重复广播同一 nonce 的交易。
- 修复：把 replay key 写入 `rabbitstore`（或至少落到 RocksDB / redb 命名空间 `compute.replay.*`），跨重启持久化。

---

## 🟠 中 — 正确性 / 协议语义

### F4. `compute_progpow` 用未初始化的 `header.hash` 作为 PoW 输入
- 文件：`Rabbit-Chain-node/crates/rabbitcore/src/consensus/mod.rs:78-82`
- 代码：
  ```rust
  fn compute_progpow(&self, header: &BlockHeader, nonce: u64) -> Hash {
      let mut data = header.hash.as_bytes().to_vec();
      data.extend_from_slice(&nonce.to_be_bytes());
      Hash::from_bytes(crate::crypto::keccak256(&data))
  }
  ```
- 问题：`header.hash` 在 `BlockHeader` 是 `#[serde(skip)]` 的缓存字段（`block/mod.rs:68`），只有少数路径（`new_with_body`、RPC 入口）写入。如果 miner 通过 RPC 直接提交未填 hash 的 header，PoW 输入退化为常量，nonce 空间无效。
- 修复：PoW 必须基于 header preimage（应已有 `encode_canonical_hash_preimage()` 之类），用 `header.compute_hash()` 的结果，或直接用 preimage bytes，而不是依赖被 serde 跳过的字段。

### F5. Mining stack `stricter_target` 与 README 「max」语义相反
- 文件：`rabbitchain-mining-stack/src/main.rs:870-871, 1232-1234`
- 代码：
  ```rust
  let effective_target = stricter_target(job_target, local_target);
  fn stricter_target(left: [u8; 32], right: [u8; 32]) -> [u8; 32] {
      if left <= right { left } else { right }  // 取较小者（更严）
  }
  ```
- 问题：README 写「实际提交阈值是 `max(本地阈值, pool job target)`」（较松者），但代码取 `min`（较严）。语义倒置：miner 会**放弃**那些能过 pool 但过不了本地阈值的 share，**少赚 share**。
- 修复：要么改 README，要么改函数语义。倾向：函数名/语义取「max target = easier to meet」更直观。

### F6. Pool 不做 share 本地校验，直接转发到节点
- 文件：`rabbitchain-mining-stack/src/main.rs:446-522` (`submit_share`)
- 问题：恶意 miner 可以塞垃圾 share，pool 不做 POW/target 校验就 POST `rabbit_submitWork` 到节点。**单 miner 即可把 node RPC 打满**（DoS 放大）。
- 修复：复用 `hash_meets_target` 与同样 preimage 校验，至少拒绝 pow 明显不达标的请求。

### F7. 节点 PoW 算法 `RandomX` / `ProgPoW` 是 stub
- 文件：`Rabbit-Chain-node/crates/rabbitcore/src/consensus/mod.rs:71-89`
- 问题：函数名暗示 RandomX / ProgPoW，实际只是 `blake3(number || nonce)` 或 `keccak256(hash || nonce)`。**没有任何内存硬性**。主网 README 没说用哪个算法；如果 `PowAlgorithm::RandomX` 真的进了配置，就是名实不符。
- 修复：要么把枚举改成 `Blake3Light / KeccakLight`，要么接真正的 RandomX / ProgPoW 实现。

### F8. Compute scheduler 不验证 `tx_id`
- 文件：`Rabbit-Chain-node/crates/rabbitcore/src/compute/scheduler.rs:205-226` (`submit`)
- 问题：`submit` 仅把 `tx.tx_id` 当 ticket 一部分存起来，不调 `has_consistent_tx_id()`。`take_pending` 也是按 `tx_id` 找。攻击者可以塞 `tx_id` 与 signing preimage 不一致的 tx 进队列，占满 `max_pending`。
- 修复：`submit` 时检查 `tx.has_consistent_tx_id()`，不一致直接 `QueueFull` 之外的 reject。

---

## 🟡 低 — 一致性 / 文档不一致 / 小遗漏

### F9. `MIN_BLOCK_REWARD` 常量（已删除）
- 文件：`Rabbit-Chain-node/crates/rabbitcore/src/lib.rs`（已删除）
- 状态：**已删除**（上一轮）。文档（`MINING.md`、`halving-schedule.md`）早已声明「无最小奖励地板」，代码里也没人引用，删除后 `cargo check --workspace` 通过。

### F10. RocksDB 并行度静默回退到 4
- 文件：`Rabbit-Chain-node/crates/rabbitstore/src/db/mod.rs:106-108`
  ```rust
  let parallelism = std::thread::available_parallelism()
      .map(|p| p.get() as i32)
      .unwrap_or(4);
  ```
- 问题：`available_parallelism` 实际几乎不会失败，但失败时静默回退到 4 jobs 而不日志。建议加 `.unwrap_or_else(|e| { warn!(...); 4 })`。

### F11. Ban 列表加载失败静默默认空
- 文件：`Rabbit-Chain-node/crates/rabbitnet/src/peer.rs:186-187`
  ```rust
  load_persisted_bans(banlist_path.as_ref()).unwrap_or_default();
  ```
- 问题：磁盘文件损坏 / 权限错误 → 全部 ban 记录丢失且无任何日志。

### F12. WebSocket listen addr 静默回退 `127.0.0.1`
- 文件：`Rabbit-Chain-node/crates/rabbitnet/src/lib.rs:1909-1913`
- 问题：与 F1 类似——`ws_listen_addr` 未配置或解析失败时回退到 loopback，节点仅本机可连 WS，外部 miner/客户端失效且无错误。

### F13. `ComputeTx.basic_sanity_check` 返回 `bool` 而非 `Result`
- 文件：`Rabbit-Chain-node/crates/rabbitcore/src/compute/tx.rs:161-183`
- 问题：与全仓 `Result<T, ComputeError>` 风格不一致，调用方 `execution.rs:153` 又包了一层 `InvalidOperation("...basic sanity check")`——错误细节被吞。
- 修复：改成 `fn basic_sanity_check(&self) -> ComputeResult<()>` 并返回具体错误（如 `MissingInputs`、`MissingOutputs`、`NonCanonicalResourceMap`）。

### F14. Chrome 钱包 `Ed25519Account.getPrivateKey()` 直接外泄私钥
- 文件：`rabbitchain-wallet-chrome/src/core/wallet/Ed25519Account.ts:39-41`
- 问题：class 暴露 `getPrivateKey(): string`，任何拿到 account 引用的代码都能拿原始私钥 hex。理想情况下 class 只暴露 `signMessage` / `signComputeTx`，根本不持有或返回私钥字符串。
- 修复：删除 getter；如导出需要，仅在 debug 模式或一次性 `export` 流程中提供。

### F15. 钱包调用栈错误信息无法区分「密码错」与「vault 损坏」
- 文件：`rabbitchain-wallet-chrome/src/background/services/WalletService.ts:98-101`
- 问题：用户输错密码与 vault 损坏显示同一条 `Invalid password or wallet not found`，误导。
- 修复：见 F2，由底层抛细分错误，上层翻译为对应文案。

### F16. 全仓 0 处 `REDLINE_ALLOW` 注解
- 现象：AGENTS.md / DESIGN_PHILOSOPHY.md / ENGINEERING_REDLINES.md 都明确说「例外必须用 `REDLINE_ALLOW` 标注原因」。但 `rg REDLINE_ALLOW crates/` 在所有源 crate 里都搜不到——意味着既没人用，也没人审计谁该用。
- 修复：要么删掉这条规则（因为实际上无人在意），要么把现有 `unwrap_or` 全部过一遍打 tag，否则规则就是空话。

---

## 通过项

- ✅ `bash scripts/no_silent_fallback.sh` 全工作区通过（红线条目 0 命中）。
- ✅ `cargo check --workspace` 全 crate 通过。
- ✅ Chrome 钱包 `Ed25519Account.signComputeTx` 与节点 `ComputeTx::signing_preimage` 字段顺序、宽度、可选项 tag、resource_map 排序一致（已逐字段比对）。
- ✅ RPC `rabbit_submitWork` 的 dedup / stale / target 检查完整。
- ✅ `BasicTxValidator` 输入/读集 domain/version/ttl/lock-script 检查完整。
- ✅ Mining-stack OTel + Prometheus 指标齐全。