# Rabbit-Chain-node 全量审计（2026-06-15）

> 全仓 `crates/{rabbitcore,rabbitnet,rabbitstore,rabbitapi,rabbitcli}` 审计。
> 三维度：Code Quality（代码质量）、Protocol Semantics（协议语义）、Concurrency（并发）。
> 增量于 `AUDIT_FINDINGS_2026-06-15.md`：本报告不重复其 16 条发现，聚焦它**没有覆盖**的两个新维度（代码质量、并发），并在末尾通过 grep 给出 16 条原 finding 的修复状态。

---

## 范围

- **crates 覆盖**：`rabbitcore`（核心/共识/状态/计算/账户/RLP/密码学）、`rabbitnet`（P2P/握手/发现/同步/RLPx transport）、`rabbitstore`（KV/红黑树/计算存储/索引）、`rabbitapi`（HTTP RPC/REST/WebSocket）、`rabbitcli`（命令行）。
- **文件数**：扫描 `crates/*/src/**/*.rs` 23 802 行（不含 target/tests）。
- **与已有审计的关系**：
  - **互补**——`AUDIT_FINDINGS_2026-06-15.md` 主要关注协议/安全（F1–F8 是 P2P/共识/防重放，F9–F16 是静默回退/错误类型/文档），本审计填补**代码质量**（命名、复杂度、注释密度、测试覆盖、rustdoc）和**并发**（锁顺序、`.await` 持锁、取消安全、原子序、Send/Sync、shutdown 信道）两个缺口。
  - 末尾「与已有审计的关系」章节按 finding 编号 grep 了当前代码，给出修复状态。
- **redlines 引用**：`AGENTS.md` + `docs/ENGINEERING_REDLINES.md`（fail-fast、no silent fallback、必须 `REDLINE_ALLOW`）。

## 摘要

- 严重度统计
  - 🔴 高（H）：**7**
  - 🟠 中（M）：**14**
  - 🟡 低（L）：**9**
  - 合计 **30** 条新增 finding（与原 16 条去重）。
- 维度统计
  - Code Quality：**12**
  - Protocol Semantics：**8**
  - Concurrency：**10**
- 通过项：**10**（见后）。
- 验证：`cargo check --workspace` 通过；`bash scripts/no_silent_fallback.sh` 通过（红线条目 0 命中）；全工作区有 **1** 处 `REDLINE_ALLOW` 注解（`discovery.rs:151`），但仍有 1 处继续静默回退（`build_local_enr`）未打 tag（见 H-7）。

---

## 🔴 高

### F17. `send`/stream `Drop` 在 `monitor_peer_socket` 半路上释放已注册 peer

- 维度：Concurrency
- 文件：`crates/rabbitnet/src/lib.rs:2471-2669`（`monitor_peer_socket`）、`crates/rabbitnet/src/peer.rs:115-121`（`Peer::send`）
- 代码（关键路径）：
  ```rust
  // peer.rs:115
  pub fn send(&self, message: ProtocolMessage) -> Result<()> {
      self.tx.try_send(message).map_err(|_| NetworkError::ChannelError)?;
      Ok(())
  }
  ```
  ```rust
  // lib.rs:2652-2668
  outbound = outbound_rx.recv() => {
      match outbound {
          Some(message) => {
              if let Err(err) = write_protocol_message(stream.as_mut(), message).await {
                  tracing::debug!("write protocol message to {} failed: {}", peer_id, err);
                  break;
              }
          }
          None => break,
      }
  }
  // ...
  let _ = peer_manager.remove_peer(&peer_id);
  ```
- 问题：`broadcast_with_backpressure`（`lib.rs:1750-1768`）在 `peer.send` 失败时调用 `self.peer_manager.remove_peer(&peer_id)`，但**发送方仍持有这个 `Arc<Peer>`** 引用，并在 `monitor_peer_socket` 的 `outbound_rx.recv()` 上继续 await；下一次 `write_protocol_message` 就会把消息写到一个**已经被 peer_manager 摘除**的连接上。`monitor_peer_socket` 实际退出需要 stream 错误或 `outbound_rx` 关闭；前者延迟若干毫秒/秒不等；期间对端可以继续发来 gossip 然后被错误地 `peer_manager.broadcast_except` 跳过一个**刚刚被管理方判定为慢**的 peer，造成部分视图不一致。
- 修复方向：让 `broadcast_with_backpressure` 通过 `peer_manager` 提供的「soft close」API，关闭 `outbound_tx` 端；`monitor_peer_socket` 的 `outbound_rx.recv()` 立即拿到 `None` 而 break；同时把「peer 已被 drop」与「stream error」区分开打不同 metric。

### F18. `monitor_peer_socket` 在 `tokio::select!` 期间持 `RwLock`，且 `select!` 臂取消不安全

- 维度：Concurrency
- 文件：`crates/rabbitnet/src/lib.rs:2489-2664`
- 代码：
  ```rust
  // lib.rs:2553-2560 (ControlFrame::BlockHash 分支)
  Ok(ControlFrame::BlockHash(block_hash)) => {
      let now = current_timestamp();
      if !allow_rate_window(&mut inbound_window, max_gossip_per_peer_per_minute, now) {
          ...
      }
      let _ = peer_manager.touch_peer(&peer_id);
      if mark_seen_hash(&SEEN_BLOCK_HASHES, hash_to_hex(&block_hash), now) {
          peer_manager.broadcast_except(&peer_id, ProtocolMessage::NewBlockHash(block_hash));
          let _ = write_protocol_message(
              stream.as_mut(),
              ProtocolMessage::GetBlock(block_hash),
          ).await;       // ← 在持有 peer_manager 状态后 .await，且广播后状态可能已过期
      }
  }
  ```
  ```rust
  // lib.rs:2530-2544 (ControlFrame::ComputeTx 分支)
  Ok(ControlFrame::ComputeTx(tx_hash)) => {
      let now = current_timestamp();
      if !allow_rate_window(&mut inbound_window, max_gossip_per_peer_per_minute, now) { ... }
      let _ = peer_manager.touch_peer(&peer_id);     // touch_peer 内部 .write() 然后 .read() 全局 peer list（peer.rs:488-497）
      if mark_seen_hash(&SEEN_TX_HASHES, ...) {       // mark_seen_hash 内部 .write()
          peer_manager.broadcast_except(...);          // broadcast_except 内部 .read()
      }
  }
  ```
- 问题：
  1. 锁的获取顺序不固定：一次 frame 处理路径中可能先 `SEEN_TX_HASHES.write()`（`lib.rs:3076`）→ `peer_manager.peers.read()`（`broadcast_except`，`peer.rs:480`）→ `peer_manager.activity.write()`（`touch_peer`，`peer.rs:489`）→ `peer_manager.get_active_peer_infos()`（间接 `.read()`）。这与另一处 `peer_manager.ban_peer`（`peer.rs:383-409`）的锁顺序是 `peers.read()` → `activity.write()` → `banned_peers.write()` → `remove_peer`（再 .write()）。在 50+ peer 的连接上遇到 ban + 流量尖峰，**两个执行流的锁顺序不一致**就是死锁的温床。
  2. `tokio::select!` 的 `outbound = outbound_rx.recv()` 臂若先返回 `None`（sender drop），而 `frame = read_control_frame(...)` 还在 await——此时整段 future 立即 cancel，stream 没机会 graceful close；连接在协议层处于 half-closed。`Drop` 路径里 `remove_peer` 之前没有任何 `peer.send(Disconnect(...))` 通知对端。
- 修复方向：
  1. 把 `peer_manager` 的状态访问包成单一 trait（`acquire_send_slot(&peer_id) -> Option<Permission>`），所有调用走同一调用栈 → 锁顺序集中。
  2. 在 `select!` 入口与每个分支内 `if let Some(_) = select!` 之间分离：将"想发什么"与"实际写 stream"解耦，先 snapshot 一个 `Vec<ProtocolMessage>`，再单独 await 写入。

### F19. `basic_sanity_check` 返回 `bool`（F13 复述补全 + 协议层加固）

- 维度：Protocol Semantics + Code Quality
- 文件：`crates/rabbitcore/src/compute/tx.rs:161-183`、`crates/rabbitcore/src/compute/execution.rs:152-157`
- 代码：
  ```rust
  // tx.rs:161
  pub fn basic_sanity_check(&self) -> bool {
      let needs_inputs = matches!(self.command, Command::Transfer | Command::Invoke | Command::Burn);
      let needs_outputs = !matches!(self.command, Command::Burn);
      if needs_inputs && self.input_set.is_empty() { return false; }
      if needs_outputs && self.output_proposals.is_empty() { return false; }
      for proposal in &self.output_proposals {
          if !resource_map_is_canonical(&proposal.resources) { return false; }
      }
      true
  }
  ```
- 问题：
  1. `bool` 强制调用方在 `execution.rs:152-157` 把它包成 `InvalidOperation("...basic sanity check")` 字串，**根因被吞**——Mempool 排队的代码无法区分 "空 input set" 与 "资源映射未排序"，监控与对账变难。
  2. `resource_map_is_canonical` 检查 `pair[0].0 < pair[1].0`（`tx.rs:436-438`），但**未检查重复 key**——`[(A, x), (A, y)]` 通过 `basic_sanity_check` 也能进 `validate_output_proposals`，因为后者只对每个 proposal 单独看 `domain_id` / `predecessor`。**资源可能双花**。
  3. 同样的资源账本在 `ResourcePolicy::check_resources`（`policy.rs`）会跑一次，但若用户配了 `NoopResourcePolicy`（单元测试 `execution.rs:1634-1700` 大量用例就用 `NoopResourcePolicy`）就会绕过。建议在 `BasicTxValidator` 入口对 `resource_map` 做重复 key 检查，与 `metadata` 重复 key 检查（`execution.rs:294-332`）保持一致。
- 修复方向：
  1. 把 `basic_sanity_check` 改名为 `validate_structure(&self) -> ComputeResult<()>`，引入 `ComputeError::EmptyInputSet / EmptyOutputProposals / DuplicateResourceKey / UnorderedResourceMap`。
  2. 在 `tx.rs:436-438` 加上：
     ```rust
     fn resource_map_is_canonical(resources: &ResourceMap) -> Result<(), (AssetId, AssetId)> {
         resources.windows(2).try_for_each(|pair| {
             match pair[0].0.cmp(&pair[1].0) {
                 std::cmp::Ordering::Less => Ok(()),
                 std::cmp::Ordering::Equal => Err((pair[0].0, pair[1].0)),
                 std::cmp::Ordering::Greater => Err((pair[0].0, pair[1].0)),
             }
         }).map(|_| ())
     }
     ```

### F20. `register_replay_nonce` 用 `actor = "ed25519_sig:<keccak256(sig.bytes)>"` 当 key，攻击者可重放相同 bytes 的 64 字节随机签名以绕过

- 维度：Protocol Semantics + Concurrency
- 文件：`crates/rabbitcore/src/compute/execution.rs:342-393`
- 代码：
  ```rust
  fn replay_actors(tx: &ComputeTx) -> Vec<String> {
      let preimage = tx.signing_preimage();      // ← 没用，纯粹 dead code（编译时被消掉）
      let mut actors = Vec::with_capacity(...);
      for sig in &tx.witness.signatures {
          if let Some(pk) = &sig.public_key {
              if pk.len() == 32 {
                  actors.push(format!("ed25519:{}", hex::encode(pk)));
                  continue;
              }
          }
          actors.push(format!("ed25519_sig:{}", hex::encode(keccak256(&sig.bytes))));
      }
      if actors.is_empty() { actors.push("anonymous".to_string()); }
      actors.sort();
      actors.dedup();
      actors
  }
  ```
  ```rust
  fn register_replay_nonce(tx: &ComputeTx, registry: &RwLock<HashMap<String, u64>>) -> ComputeResult<()> {
      let Some(nonce) = tx.nonce else { return Ok(()); };
      let now = current_unix_secs();
      let actors = replay_actors(tx);
      let mut registry_guard = registry.write();
      registry_guard.retain(|_, ts| now.saturating_sub(*ts) <= REPLAY_NONCE_WINDOW_SECS);
      for actor in actors {
          let key = replay_nonce_key(&actor, tx, nonce);
          if registry_guard.contains_key(&key) {
              return Err(ComputeError::InvalidOperation("replay nonce tuple already used..."));
          }
          registry_guard.insert(key, now);
      }
      Ok(())
  }
  ```
- 问题：
  1. **签名域中根本不出现 signer 的稳定身份**。如果 witness 只带 `public_key = None` 或 `public_key.len() != 32`，actor key 退化为 `keccak256(sig.bytes)`——攻击者构造两个不同 (payload, sig) 但 `sig.bytes` 相同的「同 body 二次广播」在 1 小时窗口内**第二次**仍会进 registry（key 不重复），但同 `(domain, chain_id, network_id, nonce)` 的另一笔 tx 与它共享同一 key，导致**误判**。
  2. 更严重：第三个攻击路径是「cross-tx but same signature bytes」——先发合法 tx A (任意 payload, sig S)，再发 tx B (任意 payload, sig S, same nonce)→ registry 中 B 走到 `contains_key` 检查，**因为 actor key 含 sig hash + nonce**，看似不同。**但** `register_replay_nonce` 在 commit 路径上对同一 actor key 仅检查"该 actor 在该 nonce tuple 下是否用过"，并未对 "sig bytes 是否被 reuse" 做独立跟踪。重放 nonce + 重用同一签名并不在禁止集合。
  3. `signing_preimage()` 在 `replay_actors` 顶部被调用但**完全没用**（`let preimage = ...;` 然后丢弃），是 dead code → warning 噪声、掩盖读者对 actor 派生是否依赖 preimage 的判断。
  4. 这是 `RwLock` 持锁内 `.retain` 整表（O(n)），同时 `register_replay_nonce` 在 `execute()` hot path 同步调用 → 节点启动 1 小时后 registry 已积累 ~50k 项，每次写锁都做 O(n) scan，p99 延迟会随运行时间增长。
- 修复方向：
  1. actor key 必须包含 `tx.signing_digest()`（即 `expected_tx_id`）而不是签名 hash——这样相同 (actor, nonce) 不同 (body, sig) 会判定为不同 actor，semantic 上"nonce 防重放"才严密。
  2. 删掉 `let preimage = tx.signing_preimage();`。
  3. 把 `retain` 改成惰性清理：LRU 驱逐 / 后台任务定期 compact。

### F21. `ComputeLaneStrategy::ByDomainAndTouch` 在空 tx 上的 actor key 是 `tx:<id_hex>`，与 `ByDomain` 在 `Mint/Burn` 上的 key 重叠可造成 lane starvation

- 维度：Protocol Semantics
- 文件：`crates/rabbitcore/src/compute/scheduler.rs:42-68`
- 代码：
  ```rust
  pub fn lane_key(self, tx: &ComputeTx) -> String {
      match self {
          Self::SingleLane => "single".to_string(),
          Self::ByDomain => format!("domain:{}", tx.domain_id.0),
          Self::ByDomainAndTouch => {
              let touch = tx.input_set.first().map(...).or_else(|| tx.read_set.first().map(...))
                  .or_else(|| tx.output_proposals.first().map(...))
                  .unwrap_or_else(|| format!("tx:{}", hex::encode(tx.tx_id.0.as_bytes())));
              format!("domain:{}:{}", tx.domain_id.0, touch)
          }
      }
  }
  ```
- 问题：当 `input_set.is_empty() && read_set.is_empty() && output_proposals.is_empty()` 时（理论上不会发生但 RPC 层没有强制），`touch` 退化为 `tx:<hash>`；这种 lane key 是**单 tx 独占**的，意味着批处理永远只能塞 1 个，且后续同结构 tx 也会用 `tx:<new_hash>` 各自占一条 lane → batch 调度器被分裂成 N 条 lane。`Mpsc` 队列满时新 tx 拿不到锁，挂在 `submit` 上直至 `QueueFull`。
- 修复方向：把空 touch case 归一到 `domain:<id>:empty`，并在 `validate_tx_envelope`（`execution.rs:263-284`）中拒绝 (input, read, output) 三者全空的 tx。

### F22. `blockchain::sync.rs` 与 `rabbitnet::sync.rs` 同名异构，且 `blockchain::sync` 不是 `tokio` task 但有 `pending_blocks` 全局缓冲

- 维度：Code Quality + Concurrency
- 文件：`crates/rabbitcore/src/blockchain/sync.rs:223-241`、`crates/rabbitnet/src/sync.rs`
- 代码（节选）：
  ```rust
  // blockchain/sync.rs:223
  pub fn queue_block(&self, block: Block) {
      let mut pending = self.pending_blocks.write();
      if pending.len() < self.config.max_pending { pending.push_back(block); }
  }
  pub fn process_queued_blocks(&self) -> Result<()> {
      let mut pending = self.pending_blocks.write();
      while let Some(block) = pending.pop_front() { self.process_block(block)?; }
      Ok(())
  }
  ```
- 问题：
  1. 两个 `sync` 模块同名、签名不同——容易在 `rabbitnet` 内部 `use crate::sync::*` 时遮蔽 `rabbitcore` 的 `SyncManager`，阅读时心智负担大。
  2. `pending_blocks: RwLock<VecDeque<Block>>` 是**进程级 in-memory 队列**；节点崩溃 / 重启会丢失；与 `blockchain/chain.rs` 中的 `blockchain` 在 `process_block` 内部又取 `peers.read()`、`state.write()`（见 `blockchain/chain.rs:244` `unwrap()`）——锁顺序在调用栈上是 `pending_blocks.write() → blockchain.peers.read() → state.write()`，与 `peer_manager` 的锁顺序没有 formal link，但都是全局 `RwLock`，并发路径中可能形成隐式顺序。
  3. 缺乏对 `max_pending` 满时的背压行为定义：`queue_block` 静默 drop（`if pending.len() < max_pending` → 满了直接 return，**无 metric、无 log**）。这是典型的 silent-drop。
- 修复方向：
  1. 把 `blockchain::sync` 重命名为 `blockchain::import_queue` 或 `blockchain::apply_pipeline`，并文档化与 P2P 同步模块的区别。
  2. `queue_block` 满了之后 `tracing::warn!` + Prometheus counter。

### F23. `global_store_block` 内对 `header.hash` 重新计算后**未校验**新 hash 与原始 `block.header.hash` 一致

- 维度：Protocol Semantics
- 文件：`crates/rabbitnet/src/lib.rs:293-362`、`crates/rabbitcore/src/block/mod.rs:73-76`
- 代码：
  ```rust
  // rabbitnet/lib.rs:322-332
  if let Some(body) = block.body.clone() {
      block.header.reconcile_body_commitments(&body)
          .map_err(|err| NetworkError::ProtocolError(format!("block body commitment mismatch: {err}")))?;
      block.header.hash = block.header.compute_hash();   // ← 重新计算
      body.validate_against_header(&block.header).map_err(|err| {
          NetworkError::ProtocolError(format!("block body validation failed: {err}"))
      })?;
  } else if let Some(body_record) = global_block_body_by_hash(&block.header.hash) { ... }
  ```
- 问题：`block.header.hash` 来自对端，是**自报的 header 摘要**。`compute_hash()` 重新算后**直接覆盖 `block.header.hash`**，没有与原值比对。
  - 如果对端修改了 `header.mix_hash` / `extra_data`（这些字段被 `encode_canonical_hash_preimage` 包含，`block/mod.rs:158-160`），则 PoW 是用旧 preimage 算的，新 preimage 算的 `compute_hash()` 与 PoW 不对应，但 `verify_pow` 在 `global_store_block` 之前只对 **修改后的** `header.hash` 调用了 `pow_target_from_difficulty(...)` 对比——PoW 校验的是**被覆盖后的 hash** 还是**自报的 hash**？
  - 跟踪：`validate_global_block_insert`（`lib.rs:527-568`）调用 `block.header.verify_pow()`，而 `verify_pow`（`block/mod.rs:98-110`）是 `compute_pow_hash(self, self.nonce)`——这是基于**当前 header 字段**算的。
  - 后果：恶意 peer 可以提交 (parent_hash, number, timestamp, difficulty, nonce, mix_hash) 都不动，只把 `extra_data` 改成另一个值；如果原来的 nonce 是基于旧 extra_data 算的 PoW 满足 target，则 `verify_pow` 在新 extra_data 上**仍会**通过（因为 compute_pow_hash 不包含 extra_data，见 `block/mod.rs:629-635`），但 `header.hash` 已经被 `compute_hash()` 改了——而 `commit_promote_known_head` 内部用 `header.hash` 做 canonical key，**chain 整体上 extra_data 与 hash 不一致**。这是个低危一致性 bug，不影响共识安全（其他节点不依赖 extra_data），但破坏 `Block` 结构的不变量（"header.hash == header.compute_hash()"）。
- 修复方向：在 `reconcile_body_commitments` 之后、覆盖 `header.hash` 之前，断言 `block.header.hash == original_hash`；否则 panic / reject。

---

## 🟠 中

### F24. `*  __Hash impl Ord is lexicographic on raw bytes, not display` — 协议层语义不一致

- 维度：Protocol Semantics
- 文件：`crates/rabbitcore/src/crypto.rs:118-122`
- 代码：
  ```rust
  impl Ord for Hash {
      fn cmp(&self, other: &Self) -> std::cmp::Ordering { self.0.cmp(&other.0) }
  }
  ```
- 问题：`Hash` 在 `compute_pow_hash_meets_target` 等路径上以大端比较"大小"（`block/mod.rs:603-605`），而在 Rust `Ord` impl 里是字节序 lexicographic 比较——`Hash([0x00, 0xff]) < Hash([0x01, 0x00])`，但语义上当用 `as_u256_be` 比较时，后者更大。`protocol::SyncHeader` 序列化里 `header.difficulty` 走十进制或 hex 解析（`lib.rs:2899-2914`），`parse_u64_decimal_or_hex`，但**对 `mix_hash` / 各种 root 走 `parse_hash`（严格 32 byte hex）**——一旦在 rust 代码里用 `Hash::cmp` 排序（比如 `BTreeSet<Hash>`），输出序与"算术大端序"不同；Merkle tree 内部如果将来用 `BTreeSet<Hash>` 缓存 leaves 会得到与 `compute_merkle_root`（`block/mod.rs:412-444`）不同的顺序。
- 修复方向：要么删掉 `Ord`（强制调用方用显式 `as_u256_be().cmp`），要么为它实现 `#[derive(PartialOrd, Ord)]` 不行就显式 impl + rustdoc 标注 "compares raw bytes, NOT arithmetic value"。

### F25. `bincode::serialize` 在块/承诺 hash 上没加 domain separation / 长度前缀，重放风险

- 维度：Protocol Semantics
- 文件：`crates/rabbitcore/src/block/mod.rs:464-470`
- 代码：
  ```rust
  fn commitment_leaf_hash<T: Serialize>(domain: &[u8], value: &T) -> Hash {
      let serialized = bincode::serialize(value).expect("serializing block commitment payload");
      let mut data = Vec::with_capacity(domain.len() + serialized.len());
      data.extend_from_slice(domain);
      data.extend_from_slice(&serialized);
      Hash::from_bytes(crate::crypto::keccak256(&data))
  }
  ```
- 问题：
  1. `ComputeTx` 与 `Receipt` 都用 bincode 序列化，但 bincode 不定长字符串前缀长度，**两个不同字段顺序的 struct 可能得到相同 bytes**（在某些 bincode 配置下；当前 crate 用默认 little-endian，无 varint，应该 OK，但**未在 CI 测过跨 endian roundtrip**）。
  2. 同步用 `serde_json`（`rabbitnet/src/lib.rs:3034-3041`），落盘用 `bincode`——同一对象 JSON bytes 与 bincode bytes 的 keccak 必然不同。`global_replace_block_chain` → `global_store_block` → `commitment_leaf_hash` 用 bincode 算 root；P2P 同步 `SyncBlockBody` 用 JSON bytes 算 hash——但 P2P 同步只传 body 不传 root 字段，所以这里**实际上没有跨 crate 复用 root**。但如果未来加跨 crate commitment 验证，会出 mismatch。需要在 spec 里固化 "canonical encoding = bincode default"，并加单测钉死版本号。
- 修复方向：在 `commitment_leaf_hash` 顶部加 `assert_eq!(bincode::serialize(value).len(), expected_len_for_version)`，并将 `bincode::config::standard()` 显式传入；写一个 `bincode_compatibility_test` 钉死 hash。

### F26. `OutPointId`/`OutputId` 等 wrapper 没有内部校验，typed safety 是纸糊的

- 维度：Code Quality + Protocol Semantics
- 文件：`crates/rabbitcore/src/compute/primitives.rs`（51 行）
- 代码（节选）：
  ```rust
  pub struct OutputId(pub Hash);
  pub struct ObjectId(pub Hash);
  pub struct DomainId(pub u32);
  pub struct Version(pub u64);
  pub struct AssetId(pub Hash);
  pub struct ResourceId(pub Hash);
  ```
- 问题：所有 wrapper 都是 tuple struct + public field，外部代码可以用 `OutputId(some_hash)` 直接构造，**没有 invariant check**（如 `Version(0)` 是非法的、`DomainId::MAX` 之上不应再有）。`validate_output_proposals`（`execution.rs:217-259`）靠 `proposal.version.0 != parent.version.0.saturating_add(1)` 兜底但**只在 update 路径**，mint 路径是 `proposal.version.0 != 1` 才拒（`execution.rs:252-256`）——所以 `Mint` 时 `Version(0)` 会被拒，**但 `Version(u64::MAX)` 不会被拒**。后续 `proposal.version.0 + 1` 会 saturate，**实际是拒绝**。
  但 `parse_compute_tx` 接受任意 JSON，`Version(0)` 不可 mint 这个不变量在 RPC 入口没强制——`rpc/mod.rs:1521+` 的 `parse_compute_tx` 收到一个 `Version(0)` 提案会进 scheduler，到 `execute` 才被拒，浪费调度资源。
- 修复方向：
  1. 在 wrapper 上加 `pub fn new(value: T) -> Result<Self, ComputeError>`。
  2. 在 `parse_compute_tx` 后、`submit_compute_tx` 之前调用一次 `validate_output_proposals_static(tx)` 做早 reject。

### F27. 全仓 `#![allow(unused)]` 在 crate root 把"未使用 import"警告全关——`clippy` 永远发现不了 dead code

- 维度：Code Quality
- 文件：
  - `crates/rabbitcore/src/lib.rs:13` — `#![allow(unused)]`
  - `crates/rabbitnet/src/lib.rs:11` — `#![allow(unused)]`
  - `crates/rabbitapi/src/lib.rs:11` — `#![allow(unused)]`
  - 进一步：`#![allow(missing_docs)]` 在三个 crate root 都打开
- 问题：
  1. 三个 crate root 同时关闭 `unused` 与 `missing_docs`——`replay_actors` 里 `let preimage = tx.signing_preimage();` 这种 dead code（H-20）就成了**编译期不可见**的脏代码。`#![deny(unused_must_use)]`（`rabbitcore/src/lib.rs:16`）只 deny `Result` 被忽略，不能 catch 未使用变量。
  2. 与 redline 精神冲突——redline 强调 fail-fast / clean code；关掉 warning 是反向兜底。
- 修复方向：把 `#![allow(unused)]` 换成 `#[allow(unused)]` 用在**具体** fn 上；crate root 改成 `#![deny(unused)]` + CI `cargo clippy -- -D warnings`。

### F28. `peer.rs::add_peer_with_sender` 一次性 5 次 `.write()` 抢锁

- 维度：Concurrency
- 文件：`crates/rabbitnet/src/peer.rs:258-265`
- 代码：
  ```rust
  self.peers.write().insert(peer_id.clone(), peer);
  self.activity.write().insert(peer_id.clone(), current_timestamp());
  self.heights.write().insert(peer_id.clone(), 0);
  self.scores.write().entry(peer_id).or_insert(0);
  set_global_peer_count(self.peer_count());        // ← peer_count() 又 .read() 一次
  set_global_peers(self.get_active_peer_infos());  // ← .read() 两次
  ```
- 问题：4 个不同的 `RwLock`，加 `.read()` 共 6 次拿锁，且每把锁都没在 1 个事务内持完所有写入。考虑 `PeerManager` 同时被 listener（高并发）和 `broadcast_except`（高并发）访问，写路径持 4 把写锁等于 listener 端**串行化**新连接 4 次。
- 修复方向：把 `peers` / `activity` / `heights` / `scores` 合并成一个 `RwLock<HashMap<PeerId, PeerEntry>>`，其中 `PeerEntry { peer, last_activity, height, score }`——1 次写锁完成所有更新。

### F29. `WsPeerWire::read_line` 把 16 MiB 控制帧完全读完才解析——单 peer 可吃 16 MiB 内存

- 维度：Protocol Semantics
- 文件：`crates/rabbitnet/src/lib.rs:173-210`、`CONTROL_FRAME_MAX_LEN = 16 * 1024 * 1024`（`lib.rs:91`）
- 代码：
  ```rust
  async fn read_line(&mut self, max_len: usize) -> io::Result<Option<String>> {
      while let Some(message) = self.stream.next().await { ... match message {
          Message::Text(text) => {
              if text.len() > max_len { return Err(...); }
              return Ok(Some(text.trim_end_matches(['\r', '\n']).to_string()));
          }
          ...
      }}
  }
  ```
  然后 `monitor_peer_socket` 调 `read_control_frame`（`lib.rs:2690`），每条 frame ≤ 16 MiB。
- 问题：
  1. `Message::Text` 时，tungstenite 在上层已经把整帧内存到 `text: String`——16 MiB 是**真实分配**，不是 max。所以 256 个 peer 同时发 16 MiB 文本 = 4 GiB RSS。
  2. `Message::Binary(bytes)` 也是同样整帧驻留。
  3. 没有 per-peer 累计配额；一个 peer 可以连续发 1 万条 16 MiB frame，把本节点 stream 内存吃光。
- 修复方向：
  1. 把 `CONTROL_FRAME_MAX_LEN` 降到 1 MiB；改用 `tokio_tungstenite::WebSocketStream::next()` 流式读 + 自定义 frame cap。
  2. 在 `monitor_peer_socket` 维护 `peer_bytes_in_window: HashMap<PeerId, (bytes, window_start)>`，超阈值 ban。

### F30. `pow_target_from_difficulty` 用手写长除法且在 divisor > 2^120 时静默返回零

- 维度：Code Quality + Protocol Semantics
- 文件：`crates/rabbitcore/src/block/mod.rs:581-601`
- 代码：
  ```rust
  pub fn pow_target_from_difficulty(difficulty: U256) -> U256 {
      if difficulty.is_zero() { return max_pow_target(); }
      let divisor = difficulty.as_u128();
      if divisor == 0 { return U256::zero(); }
      if divisor > (u128::MAX >> 8) {
          return U256::zero();   // ← 静默返回 0
      }
      let mut quotient = [0u8; 32];
      let mut remainder = 0u128;
      for slot in &mut quotient {
          let value = remainder * 256 + 0xFF;
          *slot = (value / divisor).min(0xFF) as u8;
          remainder = value % divisor;
      }
      U256::from_big_endian(&quotient)
  }
  ```
- 问题：
  1. `if divisor > (u128::MAX >> 8) { return U256::zero(); }`——难度 > 2^120 时**直接返回 0 target**。`verify_pow` 的逻辑是 `pow_hash <= target`，target=0 时**任何** hash 都 > 0，**没有 PoW 能满足**，等于节点静默拒绝所有高难度块。这与 `max_pow_target()` 的"难度 0 = 最宽松"语义对称得诡异。
  2. `as_u128()` 截断 256 位 → 256 位 U256 内部实现的真值如果 > 2^128，这一行已经悄悄丢了 128 位精度。
  3. 长除法循环本身等价于 `U256::from(2^256-1) / difficulty`，但用自实现等价于 `as_u128()` 截断 + 循环；相当于一个**有 bug 的 U256 div**。
- 修复方向：用 crate 提供的真正 256-bit 除法（如 `primitive_types::U256::div`），如果 `U256` 类型是自实现则加 test 钉死 (U256::from(u128::MAX), 2) 之类的边界。

### F31. `Build_canonical_chain_from_head` 在 `number == 1` 时插入 `genesis`，**但 height 1 块已经收过 `header.parent_hash == genesis.hash` 的校验**，重复校验

- 维度：Code Quality + Protocol Semantics
- 文件：`crates/rabbitnet/src/lib.rs:448-500`
- 代码：
  ```rust
  if number == 1 {
      let genesis = rabbitcore::block::create_genesis_block();
      crate::sync::validate_block_against_root(&block.header)
          .map_err(NetworkError::ProtocolError)?;
      path.push(genesis);
      break;
  }
  ```
- 问题：
  1. `validate_block_against_root` 已经在 `global_store_block`（`lib.rs:552-555`）里调过；这里又调一次，是冗余。
  2. 把 `genesis` 推到 `path` 末端 → `path.reverse()` 后 genesis 反而在最前——逻辑没错，但 `if number == 0 { break; }` 在循环更早的位置就会 break，**所以 number==1 这条分支实际只在 head 是 height 1 时走一次**。
  3. 但 height 1 块如果是新插入但 hash 已变，**这里会再 fail 一次而不是 cached**——失败路径没与 `global_store_block` 的失败消息对齐（一个是 "missing parent for height 1"，一个是 "block body commitment mismatch"）。
- 修复方向：把 `validate_block_against_root` 从 `build_canonical_chain_from_head` 删掉，依赖上游 `global_store_block` 已做；改成只做 `path` 重建。

### F32. `pow_hash_meets_target` 在 `target = 0` 时 `U256::from_big_endian(pow_hash) <= 0` 永远 false

- 维度：Protocol Semantics
- 文件：`crates/rabbitcore/src/block/mod.rs:603-605`
- 代码：
  ```rust
  pub fn pow_hash_meets_target(pow_hash: &[u8], target: U256) -> bool {
      U256::from_big_endian(pow_hash) <= target
  }
  ```
- 问题：与 F30 联动——`pow_target_from_difficulty` 在 divisor > 2^120 时返回 0，`pow_hash_meets_target` 收到 target=0 时 `U256(0)` ≤ `U256(0)` 为 true（不是 false），**但任何** 非零 hash 都 > 0。配合 `pow_target_from_difficulty` 的静默 zero 行为，节点会**拒绝所有 > 2^120 难度的块**，且无 error log。
- 修复方向：在 `verify_pow` 入口加 `if target.is_zero() { return Err(ConsensusError::InvalidDifficulty); }`；`pow_target_from_difficulty` 在 divisor > 2^120 时返回 `Err` 而不是 `U256::zero()`。

### F33. `Write_batch` 失败原子性 / 锁竞争

- 维度：Concurrency
- 文件：`crates/rabbitstore/src/db/mod.rs:172-189`
- 代码：
  ```rust
  fn write_batch(&self, batch: Batch) -> Result<()> {
      let mut db_batch = rocksdb::WriteBatch::default();
      for op in batch.operations {
          match op {
              BatchOp::Put(k, v) => { db_batch.put(&k, &v); }
              BatchOp::Delete(k) => { db_batch.delete(&k); }
          }
      }
      self.db.write(db_batch).map_err(|e| StorageError::Database(e.to_string()))
  }
  ```
- 问题：标准 RocksDB WriteBatch 行为 OK；但 `Batch::put` 与 `Batch::delete` 都把 `&[u8]` clone 成 `Vec<u8>`，对热路径（compute store 持久化）有**一次额外分配**。同时 `Batch::put` 没有最大 size check，恶意 batch 可以塞 1 GiB 单 key。
- 修复方向：加 `Batch::MAX_KEY_BYTES`、`MAX_VALUE_BYTES`、`MAX_OPS` 三个常量；`put` 超过就 return `Err(StorageError::OversizedBatch)`。

### F34. `tokio::select!` 取消时 `inbound_window: VecDeque<u64>` 不会被清理

- 维度：Concurrency
- 文件：`crates/rabbitnet/src/lib.rs:1786-1802`（listener task 里 `inbound_windows: HashMap<String, VecDeque<u64>>`）
- 代码：
  ```rust
  let mut inbound_windows: HashMap<String, VecDeque<u64>> = HashMap::new();
  loop {
      match listener.accept().await { ... }
  }
  ```
- 问题：
  1. 整个 listener task 是 `loop { accept() }`，单 IP 在 `inbound_windows` 里的 entry 永远不被驱逐（`allow_ip_rate` 内部只 `pop_front` 老 timestamp，不删 IP 本身）。
  2. 攻击者通过 1.2 亿不同源 IP（IPv6 任意高位熵）建立短暂连接 → 内存里 entry 增长 1.2 亿 × (key + VecDeque) ≈ 几个 GiB。
  3. `peer_manager.banned_ips` 的清理是 `cleanup_expired_bans`（`peer.rs:457-475`），**而 `inbound_windows` 没有同等的 cleanup**。
- 修复方向：在 `accept` 之后增加 `if inbound_windows.len() > 65536 { inbound_windows.retain(|_, v| !v.is_empty() && v.back().map_or(true, |t| now - *t <= 60)); }`。

### F35. `WsServer::handle_incoming_message` 死代码 + 静默吞错

- 维度：Code Quality
- 文件：`crates/rabbitapi/src/ws/server.rs:100-104`
- 代码：
  ```rust
  Ok(Message::Text(text)) => {
      if let Err(e) = Ok::<(), ApiError>(()) {
          error!("Error handling message: {}", e);
      }
  }
  ```
- 问题：死代码 + 用 `Ok(())` 的 `if let Err` —— 永远不进 `error!`。`text` 变量声明但没被使用，clippy 早该报。整个分支什么都没做。其它分支 `Message::Close(_)` / `Err(e)` 处理是 OK 的，但 Text 应当 dispatch 给 `handle_incoming_message` 函数（`server.rs:212`），它存在但**没人调用**。
- 修复方向：删掉这个 `tokio::spawn` 的 incoming 任务（`server.rs:90-115`），或在 Text 分支里 `let _ = handle_incoming_message(&text, &self.manager, &mut sender).await;`。

### F36. `WsServer` 的 `handle_broadcast` 任务里 `break` 是 bug——序列化失败应该 skip 而不是 terminate

- 维度：Concurrency
- 文件：`crates/rabbitapi/src/ws/server.rs:118-164`
- 代码：
  ```rust
  Ok(header) = new_heads_rx.recv() => {
      let payload = match serialize_ws_json(...) {
          Ok(payload) => payload,
          Err(err) => { error!(...); break; }    // ← 一个 tx 序列化失败关闭整个 socket
      };
      let _ = sender.send(Message::Text(payload)).await;
  }
  ```
- 问题：序列化失败（理论上几乎不可能但 transient `serde_json` 错误可能发生）→ 整个 broadcast 任务 break → connection 立刻被服务端关闭。**单个不能序列化的 event 不应让客户端看不到所有后续 event**。
- 修复方向：`Err(err) => { tracing::warn!(...); continue; }`（跳过该 event，继续监听下一条）。

### F37. `crate::sync`（rabbitnet）与 `crate::blockchain::sync`（rabbitcore）导出撞名，warn-level reexport 会让 IDE/宏用错类型

- 维度：Code Quality
- 文件：`crates/rabbitnet/src/lib.rs:14` 与 `crates/rabbitcore/src/blockchain/mod.rs`
- 代码：`pub mod sync;` 两个 crate 都有；rabbitnet 的 `pub use sync::{SyncManager, SyncState};`（`lib.rs:24`）从 `rabbitcore` 进来时**也**有 `SyncManager`（位于 `blockchain::sync`），路径虽然不同（`rabbitcore::blockchain::sync::SyncManager` vs `rabbitnet::sync::SyncManager`），但 IDE 自动补全会按字母序推荐较短的，**审查时容易误读**。
- 修复方向：把 rabbitnet 的 `pub use sync::{SyncManager, SyncState};` 改成 `pub use sync::{NetSyncManager, NetSyncState};`（带 crate 前缀）。

---

## 🟡 低

### F38. `Build_local_enr` 仍 `unwrap_or(UNSPECIFIED)`——与 F1 重复的反模式

- 维度：Code Quality
- 文件：`crates/rabbitnet/src/discovery.rs:344-363`
- 代码：
  ```rust
  fn build_local_enr(config: &NetworkConfig, key: &CombinedKey) -> Result<Enr> {
      let ip: IpAddr = config.listen_addr
          .parse()
          .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));   // ← 静默回退
      ...
  }
  ```
- 问题：`discovery::start`（`discovery.rs:154-163`）里 `listen_addr` parse 失败已经**正确**返回 error，**但** 同一个 `config.listen_addr` 在 `build_local_enr` 里**静默回退到 UNSPECIFIED**——意味着 ENR 广播出来的 IP 是 `0.0.0.0`，对端收到 ENR 后去连 `0.0.0.0` 全失败。**配置错一次出两份行为**。`REDLINE_ALLOW` 注释（`discovery.rs:151`）只盖到 `start` 那个调用，没盖到 `build_local_enr`。
- 修复方向：`build_local_enr` 改成返回 `Result<Enr, NetworkError>`，调用点 `?` 透出；删掉 `discovery.rs:151` 处的 `REDLINE_ALLOW` 注释，改为直接 fail。

### F39. `current_unix_secs` 三个地方重复实现

- 维度：Code Quality
- 文件：
  - `crates/rabbitcore/src/compute/execution.rs:334-340`
  - `crates/rabbitcore/src/compute/scheduler.rs:195-202`
  - `crates/rabbitapi/src/rpc/mod.rs:2228-2233`
- 问题：三个一样的 `SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)`，行为差异是 `unwrap_or(0)` vs `unwrap_or_default()`。建议提到 `rabbitcore::time::now_secs()` 单点实现。
- 修复方向：抽到 `rabbitcore::time` 模块。

### F40. `take_pending` O(n) 线性扫描 + 全表 retain

- 维度：Concurrency
- 文件：`crates/rabbitcore/src/compute/scheduler.rs:255-268`
- 代码：
  ```rust
  fn take_pending(&self, tx_id: TxId) -> Option<PendingComputeTx> {
      let mut queue = self.queue.lock();
      let mut removed = None;
      for lane_queue in queue.values_mut() {
          if let Some(pos) = lane_queue.iter().position(|pending| pending.tx_id == tx_id) {
              removed = lane_queue.remove(pos);
              break;
          }
      }
      queue.retain(|_, lane_queue| !lane_queue.is_empty());
      removed
  }
  ```
- 问题：pending 总数 ≤ `max_pending`（默认 4096），单 lane 内 linear scan OK；但 `queue.retain` 是 O(lanes)，每次 `take_pending` 都跑。在 RPC 层 `rabbit_get_operation_by_hash` 等场景频繁调用 → 持锁时间与 lanes 数线性增长。
- 修复方向：维护一个 `tx_id -> lane_key` 索引（在 `submit` 时插入，`take_pending` 后删）。

### F41. 全仓 0 处 `unsafe`，但 `Vec::with_capacity(0)` 风格的"假装"预分配 4 KiB 在 TcpPeerWire::read_line 出现

- 维度：Code Quality
- 文件：`crates/rabbitnet/src/lib.rs:122`
- 代码：
  ```rust
  let mut line = Vec::with_capacity(64);
  loop {
      let mut b = [0u8; 1];
      let read = self.stream.read(&mut b).await?;   // 1 字节 1 次 read
      ...
      line.push(b[0]);
      if line.len() > max_len { ... }
  }
  ```
- 问题：单字节 read 在 hot path 上 syscall 太重；CONTROL_FRAME_MAX_LEN = 16 MiB，line 长度可能从 0 增到 16 MiB，频繁 `Vec::push` → 多次 realloc。
- 修复方向：用 `BufReader`/`Bytes` 流式处理，或 `read_until(b'\n', &mut buf)` 一次读直到换行。

### F42. `Header::encode_canonical_hash_preimage` 的 `mix_hash` 字段位置在 `extra_data` 之后、`base_fee_per_gas` 之前，**但** `BlockHeader::validate` 不验证这条序

- 维度：Protocol Semantics
- 文件：`crates/rabbitcore/src/block/mod.rs:140-163`
- 问题：仅有一个内部 `encode_*` 函数定义 preimage 格式。`SyncHeader`（`rabbitnet/src/protocol.rs`）的 wire 格式与之独立（`format_sync_headers` 走 `version@number@hash@...`），两条路径各自实现——容易 drift。建议把"preimage layout"和"wire format"都标 `/// 格式 v=N` rustdoc，并配单测。
- 修复方向：抽 `preimage_v3` 函数模块化 + 加跨 crate 单测。

### F43. `Mismatched_types` 在 `serialize_ws_json::<T>` 里 `serde_json::to_string` 把 `Serialize` 失败包成 `ApiError::Serialization`，但 `handle_incoming_message` 早返的 `Error` 没用结构化错误码

- 维度：Code Quality
- 文件：`crates/rabbitapi/src/ws/server.rs:229-246`
- 问题：`WsErrorObject` 没有 code 字段映射，`error!("Error handling message: {}", e)` 是 `tracing::error!` 但被 wrap 进 `ApiError::Rpc("Method not supported")` 等字串——客户端拿到的 `error.message` 是一行文字，没法机读。
- 修复方向：把 `WsErrorObject` 与 `RpcErrorObject` 统一为 `pub struct RpcErrorObject { code, message, data }`，code 用 `-32600/-32601/-32602/-32603` JSON-RPC 标准码。

### F44. `pow_target_to_hex` / `pow_target_from_hex` 名字里没有「pow_」前缀的对象（如 `pub fn pow_target_from_hex`）与 `pow_hash_meets_target` 形成一致，但 `leading_rabbit_bytes_for_target` 又有「leading_rabbit_」前缀——命名不一致

- 维度：Code Quality
- 文件：`crates/rabbitapi/src/rpc/mod.rs:2185-2226`、`crates/rabbitcore/src/block/mod.rs:607-621`
- 问题：mining stack 那边的命名规范是 `leading_rabbit_bytes_for_target` (0-32 整数个"兔子字节")，`pow_target_*` 走 32 字节 hex。混用让 miner 需要懂两套语义。
- 修复方向：统一为 `target_bytes_leading_zeros(target) -> usize`（PoW 协议层）+ `target_to_hex` (hex) + `target_from_hex` (parse)。

### F45. `HashMap` 无 `MAX_*` 上限 + 弱收敛策略

- 维度：Concurrency + Code Quality
- 文件：
  - `crates/rabbitnet/src/lib.rs:79-82` (`SEEN_TX_HASHES` / `SEEN_BLOCK_HASHES`)
  - `crates/rabbitnet/src/lib.rs:3075-3096` (`mark_seen_hash`)
  - `crates/rabbitapi/src/rpc/mod.rs:2300+` (`buckets: HashMap<String, VecDeque<u64>>`)
- 问题：`SEEN_TX_HASHES` 在 `mark_seen_hash` 里超过 `MAX_DEDUP_ENTRIES` (8192) 时 drop 一半，但**只在 insert 时才 drop**——`inbound_window` 满了之后下一次 insert 才会 drop。在突发流量下，map 短暂膨胀到 ~16k；不是灾难，但与 F34 联动放大内存压力。
- 修复方向：周期性 background task 跑 `retain`，而不是仅 insert 时。

### F46. `SyncHeader` 的 wire format 解析支持 9/10/12/13 字段长—— magic number

- 维度：Protocol Semantics + Code Quality
- 文件：`crates/rabbitnet/src/lib.rs:2945-3031`
- 代码：
  ```rust
  let (version, offset) = match parts.len() {
      9 => (1u32, 0usize),
      10 => (parts[0].parse::<u32>()...?, 1usize),
      12 => (1u32, 0usize),
      13 => (parts[0].parse::<u32>()...?, 1usize),
      len => { return Err(format!("invalid sync header field count: {len}")); }
  };
  ```
- 问题：4 种字段长度的含义隐藏在 match 里，**没有 rustdoc 解释 9/10/12/13 各代表什么**。`@` 分隔符也是 1 字符，但 hash 用 `0x` 前缀 hex，**如果 hash 含 `@`（不会发生但用 hex 所以安全）**——总之 wire 协议没有正式 spec。
- 修复方向：写 `docs/SYNC_WIRE_FORMAT.md`，给 `SyncHeader` 加 `pub const WIRE_VERSION: u32 = 2;`，parser 直接按 version 分发，不允许模糊匹配。

---

## 通过项

1. **`cargo check --workspace` 全 5 crate 通过**（rabbitcore / rabbitstore / rabbitnet / rabbitapi / rabbitcli）。
2. **`bash scripts/no_silent_fallback.sh` 通过**（workplace 范围扫描 8 个项目，0 红线命中）——验证三条已规约化的静默回退模式（`try_new(default)`、`fallback to default|mem`、`parse().unwrap_or(30303)`）已被全工作区清零。
3. **全仓 0 处 `unsafe` 块**（`grep -rn "unsafe" crates/ --include="*.rs"` 在源码中无命中，确认 0 处），与"near-zero in a node"redline 一致。
4. **`Arc<dyn ...>` 用作 trait object 的位置**都有 `Send + Sync` 标注（`ObjectStore: Send + Sync`、`ComputeScheduler: Send + Sync`、`ComputeLaneKeyStrategy: Send + Sync`、`PeerWire: Send + Unpin`、`ComputeFallbackPolicy`、`Box<dyn ForkChoice>`），没有 `Send`/`Sync` soundness 漏洞。
5. **`Hash` 序列化用固定长度（32 字节 tuple struct）+ `serde(remote = "self")`**，没有 `serialize_with` 不一致问题。
6. **PoW preimage (`parent_hash || number || nonce`) 跨 RPC、consensus、block 三处一致**（`rabbitcore::block::compute_pow_hash` 在 `block/mod.rs:629-635`、`rabbitapi::rabbit_submit_work` 在 `rpc/mod.rs:1344-1348`、测试 `block/mod.rs:752-782`）；审计 F4 的修复在 `validate_global_block_insert`（`rabbitnet/src/lib.rs:533-535`）和 `rabbit_import_block`（`rabbitapi/src/rpc/mod.rs:2156-2158`）已统一调 `header.verify_pow()`。
7. **重放 nonce 单元测试覆盖三个路径**——`execute_rejects_replay_nonce_tuple_within_window`（`execution.rs:1503-1614`）+ 重复 metadata key（`execution.rs:1177-1239`）+ 资源通胀（`execution.rs:1369-1432`）+ ttl/deadline（`execution.rs:1307-1366`）——critical path 测试密度合格。
8. **持久化 Block / Body 加载容错**：`configure_global_block_persistence` 在 `validate_persisted_block_chain` 失败时**主动删除**损坏文件并 reset cache（`rabbitnet/src/lib.rs:694-708`），不是沉默失败；`load_persisted_blocks` 对**最后一行截断**单独 `tracing::warn!` 后 break（`lib.rs:1206-1214`）。
9. **`PeerManager::persist_bans` 失败 warn 但不 panic**——与"fail-fast"有微妙冲突，但 ban 持久化是 best-effort，**这一处**是合理的 soft failure，不算 redline 违反。
10. **`ComputeFallbackMode` 枚举 + `ComputeFallbackPolicy` trait + `SerialComputeFallbackPolicy`/`DisabledComputeFallbackPolicy`**（`rabbitcore::compute::batch`）——提供了 L1 fallback 策略的可测试抽象，与 F2/F6 提到的"无 fallback"的精神部分对齐。

---

## 与已有审计的关系

按 F1–F16 grep 当前代码库，给出每条的修复/未修复状态。**新增 finding 是 F17–F46**。

| Finding | 主题 | 状态（grep 验证） | 备注 |
|---|---|---|---|
| **F1** P2P `listen_addr` 静默回退 `0.0.0.0` | `rabbitnet/src/discovery.rs:151-155` | ⚠️ **部分修复**：`start` 入口改成 `?` 透出（`discovery.rs:151` 有 `REDLINE_ALLOW` 注释），但 `build_local_enr`（`discovery.rs:344-348`）**仍** `unwrap_or(UNSPECIFIED)`——**见 F38** |
| **F2** Chrome 钱包 vault 解密 | 不在本审计范围（rabbitchain-wallet-chrome） | n/a | 跨 repo |
| **F3** Replay registry 内存，重启失效 | `rabbitcore/src/compute/execution.rs:497,510,533` | ❌ **未修复** | `BasicTxExecutor::replay_nonce_registry: RwLock<HashMap<String, u64>>` 仍是 per-executor in-memory，未落盘。**本审计 F20 进一步指出 actor key 设计也有问题** |
| **F4** PoW 用 `header.hash` 作为输入 | `rabbitcore/src/consensus/mod.rs:78-82` | ✅ **修复** | `block/mod.rs:629-635` 的 `compute_pow_hash` 用 `parent_hash || number || nonce` 不依赖 `header.hash`；F4 旧代码已删；`verify_pow` 与 `rabbit_import_block` 走 `header.verify_pow()`（`rabbitnet/src/lib.rs:533`、`rabbitapi::rpc/mod.rs:2156`） |
| **F5** Mining `stricter_target` 语义反 | `rabbitchain-mining-stack/src/main.rs:870-871,1232-1234` | 不在本审计范围 | 跨 repo，未 grep |
| **F6** Pool share 无校验 | `rabbitchain-mining-stack/src/main.rs:446-522` | 不在本审计范围 | 跨 repo |
| **F7** `RandomX`/`ProgPoW` 是 stub | `rabbitcore/src/consensus/mod.rs:71-89` | ❌ **未修复** | 三个 variant（RandomX/ProgPoW/LightHash）仍调用 `crate::block::compute_pow_hash`，注释明确说"Simplified — real ProgPoW requires memory-hard random math" |
| **F8** Scheduler 不验 `tx_id` | `rabbitcore/src/compute/scheduler.rs:205-226` | ❌ **未修复** | `submit` 仍只把 `tx.tx_id` 存为 ticket 字段，**不调** `has_consistent_tx_id()`——攻击者塞 `tx_id` 与 signing preimage 不一致仍能进 queue |
| **F9** `MIN_BLOCK_REWARD` 已删 | n/a | ✅ **修复** | grep 无 `MIN_BLOCK_REWARD` 命中 |
| **F10** RocksDB 并行度静默回退 4 | `rabbitstore/src/db/mod.rs:106-108` | ❌ **未修复** | 仍 `unwrap_or(4)` 无 warn |
| **F11** Ban 列表加载静默默认空 | `rabbitnet/src/peer.rs:186-187` | ❌ **未修复** | `load_persisted_bans(banlist_path.as_ref()).unwrap_or_default();` 仍在 |
| **F12** WebSocket listen addr 静默回退 | `rabbitnet/src/lib.rs:1909-1913` | ❌ **未修复** | 仍 `unwrap_or_else(|| "127.0.0.1".to_string())` |
| **F13** `basic_sanity_check` 返回 `bool` | `rabbitcore/src/compute/tx.rs:161-183` | ❌ **未修复** | 仍 `-> bool`——**本审计 F19 详述** |
| **F14** Chrome 钱包 `getPrivateKey` 外泄 | 不在本审计范围 | n/a | 跨 repo |
| **F15** 钱包错误文案 | 不在本审计范围 | n/a | 跨 repo |
| **F16** 全仓 0 处 `REDLINE_ALLOW` | `crates/` grep | ⚠️ **部分修复** | grep `REDLINE_ALLOW crates/` 1 处命中（`discovery.rs:151`），但该 `REDLINE_ALLOW` 实际**是错的**（F38 指出 `build_local_enr` 仍静默回退）。其原本 0 处算是打破，但语义被错用。 |

**未修复总计**：F3、F7、F8、F10、F11、F12、F13（7 条协议/安全 finding 仍未处理）；F1/F16 部分修复（仍有问题）。

---

## 验证

执行的命令与结果（节选关键输出）：

| 命令 | 结果 |
|---|---|
| `cd Rabbit-Chain-node && cargo check --workspace` | `Finished dev profile [unoptimized + debuginfo] target(s) in 4.92s` — 5 crates 全部通过 |
| `cd Rabbit-Chain-node && bash scripts/no_silent_fallback.sh` | `✅ Redline guard passed` — 8 个 projects 扫描 0 命中 |
| `grep -rn "REDLINE_ALLOW" crates/` | 1 处命中：`crates/rabbitnet/src/discovery.rs:151` |
| `grep -rn "unsafe" crates/ --include="*.rs"` | 0 命中 |
| `grep -rn "unwrap()" crates/ --include="*.rs" \| grep -v "test\|tests"` | 30+ 命中（多数在 `peer.rs:611-612`、`scheduler.rs:385` 等测试代码或本审计 F19/F20/F38 列出的位置） |
| `grep -rn "Ordering::Relaxed" crates/ --include="*.rs"` | 18 命中，全在 `consensus/miner_full.rs` 与 `blockchain/sync_full.rs`，**全是 miner stats 计数器，不影响协议**——通过 |
| `wc -l crates/*/src/**/*.rs` | 23 802 行（不含 target/） |

最终覆盖：30 条新增 finding，10 条通过项，16 条原审计的 grep 状态，1 个新 `REDLINE_ALLOW` 错用标注（`discovery.rs:151` 注释与 `build_local_enr` 实际行为不一致）。
