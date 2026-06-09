# 上线前阻断清单（Go/No-Go Checklist）

> 适用范围：RabbitChain 节点、RPC/API、存储、计算交易（UTXO Compute）功能的版本发布。
> 目标：在“真实用户 + 真实资产”场景前，明确**必须满足**的上线门槛，避免带病发布。

---

## 1. 使用方式（强制）

- 每个检查项都必须填写状态：`PASS / FAIL / WAIVED`。
- `FAIL` 项默认阻断上线。
- `WAIVED` 必须有：
  - 风险说明
  - 临时缓解措施
  - 负责人
  - 关闭截止时间（Deadline）
- 最终由发布负责人给出结论：`GO` 或 `NO-GO`。

建议在发布会议中逐项过表，不接受“口头默认通过”。

---

## 2. 阻断级检查项（Fail 即 No-Go）

### A. 代码与构建

- [ ] A1. 当前发布分支已打版本标签（tag），且 tag 与构建产物一一对应。  
  **证据**：`git tag`, CI 构建记录、制品哈希。
- [ ] A2. `cargo check --workspace` 通过。  
  **证据**：CI 日志。
- [ ] A3. `cargo test --workspace` 通过（不得跳过关键测试）。  
  **证据**：CI 测试报告。
- [ ] A4. 关键 crate（`rabbitcore/rabbitapi/rabbitstore`）无 `panic!` 热路径风险（已人工审查）。

### B. 协议与交易安全（Compute 必查）

- [ ] B1. 计算交易签名消息绑定完整（domain/command/input/read/output/payload/deadline/threshold）。
- [ ] B2. `tx_id == expected_tx_id(signing_preimage)` 校验生效。
- [ ] B3. 地址所有权输入（`Ownership::Address`）已强制签名归属校验。
- [ ] B4. 重放风险边界明确（chain/network/domain 绑定策略已确认并测试）。
- [ ] B5. 授权失败错误码返回准确且可观测（3001~3005）。

### C. 状态与存储一致性

- [ ] C1. 持久化后重启恢复一致（compute result / output / object 可重查）。
- [ ] C2. 重复提交幂等行为符合预期（duplicate request 返回稳定结果）。
- [ ] C3. 数据库后端（Mem/RocksDB/Redb）已验证目标环境配置。
- [ ] C4. 数据目录权限、容量阈值、备份策略已落实。

### D. RPC/API 稳定性

- [ ] D1. 核心 RPC 方法冒烟通过：
  - `rabbit_simulateComputeTx`
  - `rabbit_submitComputeTx`
  - `rabbit_getComputeTxResult`
  - `rabbit_getObject` / `rabbit_getOutput` / `rabbit_getDomain`
- [ ] D2. 错误码与 API 文档一致（包括分类/数值/message）。
- [ ] D3. 向后兼容评估完成（旧客户端是否受影响已确认）。

### E. 安全与合规

- [ ] E1. 完成最小安全审计（内部/外部至少一种），结论可追溯。
- [ ] E2. 无明文密钥/敏感配置提交到仓库。
- [ ] E3. 发布环境密钥托管与轮换策略明确（谁可访问、如何审计）。
  **证据**：
  - 验收清单：`docs/KEY_MANAGEMENT_ACCEPTANCE.md`
  - 演练记录：`artifacts/security/key-rotation-drill-<date>.md`
  - 审批/审计记录：访问授权、轮换审批、吊销日志

### F. 运维与可观测性

- [ ] F1. 关键指标可观测：请求量、错误率、延迟、存储读写失败率。
- [ ] F2. 日志可检索，且包含 tx_id/request_id 便于追踪。
- [ ] F3. 告警已配置并演练（服务不可用、错误率突增、DB 异常）。
- [ ] F4. 回滚方案已演练（可在 SLA 时间内恢复）。

### G. 性能与容量

- [ ] G1. 压测达到目标阈值（TPS、P95/P99、CPU/内存）。
- [ ] G2. 长稳测试（Soak Test）至少 24h（建议 72h）无致命问题。
- [ ] G3. 峰值流量下不出现不可恢复错误（如持续写失败/崩溃循环）。

---

## 3. 建议级检查项（Fail 不一定阻断，但需记录）

- [ ] S1. 文档已更新：API、部署、故障处理、升级步骤。
- [ ] S2. CLI 参数与默认值已复核（特别是 compute backend/db path）。
- [ ] S3. 发布公告模板与变更说明（changelog）已准备。
- [ ] S4. 客户支持/值班计划已排班。

---

## 4. 发布当日执行单（Runbook）

1. 锁定发布 commit/tag（禁止临时加改）。  
2. 再跑一次发布流水线（构建 + 测试 + 基础冒烟）。  
3. 预发布环境回归（至少覆盖核心交易闭环）。  
4. 生产灰度发布（小流量/单节点先行）。  
5. 观察 15~30 分钟关键指标。  
6. 无异常再逐步全量。  
7. 发布后 24 小时重点值守。

自动化门禁建议：

- 使用 `scripts/run_tests.sh` 作为统一 release gate 入口。
- CI 执行并上传 `artifacts/release-gate/go-no-go-report.md`。
- 该自动化门禁覆盖：
  - `cargo fmt --all --check`
  - `cargo check --workspace`
  - `cargo test --workspace`
  - `cargo test --workspace -- --ignored`（信息项）

---

## 5. Go/No-Go 结论模板

### 版本信息

- 版本：
- Commit：
- Tag：
- 发布负责人：
- 会议时间：

### 阻断项统计

- PASS：
- FAIL：
- WAIVED：

### 风险豁免（WAIVED）

| 项目 | 风险描述 | 缓解措施 | 负责人 | Deadline |
|---|---|---|---|---|
|  |  |  |  |  |

### 最终结论

- [ ] GO（允许上线）
- [ ] NO-GO（阻断上线）

审批：

- 技术负责人：
- 安全负责人：
- 运维负责人：
- 产品/业务负责人：

---

## 6. 当前项目的最低 Go 建议（针对现状）

在当前版本中，至少满足以下条件再考虑 GO：

1. `cargo test --workspace` 全绿；
2. compute 相关 e2e 场景在持久化后端（RocksDB/Redb）跑通；
3. 针对签名/授权失败（3001~3005）完成黑盒回归；
4. 完成一次 24h 稳定性运行并无致命告警；
5. 回滚流程可在约定时间内完成。

如果以上任一项不满足，建议 `NO-GO`。
