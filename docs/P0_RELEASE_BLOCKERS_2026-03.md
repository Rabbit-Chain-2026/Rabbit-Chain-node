# P0 发布阻塞推进板（2026-03）

> 目标：关闭 [发布就绪](book/70-operations/release-readiness.md) 中当前阻塞上线的 P0 项，并保留可审计证据。
>
> 范围：E1、E3、F1-F4、G1-G3、Rollback rehearsal。

---

## 1) 当前状态总览

| 项目 | 状态 | 负责人 | 截止时间 | 证据 |
|---|---|---|---|---|
| E1 安全审计 | TODO | TBD | TBD | 审计报告链接 + 问题清单 |
| E3 密钥托管/轮换 | TODO | TBD | TBD | 演练记录 + 审批记录 |
| F1 指标可观测 | IN_PROGRESS | TBD | TBD | metrics 面板截图 + 指标清单 |
| F2 日志可追踪 | IN_PROGRESS | TBD | TBD | tx_id/request_id 检索结果 |
| F3 告警演练 | TODO | TBD | TBD | 告警触发与恢复记录 |
| F4 回滚演练 | TODO | TBD | TBD | 回滚步骤与耗时 |
| G1 压测阈值 | TODO | TBD | TBD | TPS/P95/P99 报告 |
| G2 Soak 长稳 | TODO | TBD | TBD | 24h/72h 运行报告 |
| G3 峰值可恢复 | TODO | TBD | TBD | 故障注入 + 恢复结果 |

---

## 2) 本轮落地（已完成）

1. 已固化自动化全链路联调脚本：`scripts/full_chain_e2e.sh`
2. 已接入 CI 自动执行：`.github/workflows/full-chain-e2e.yml`
3. 已生成联调证据文档：`docs/FULL_CHAIN_E2E_2026-03-07.md`

---

## 3) 阻塞项执行清单（可直接照跑）

### E1 安全审计（最小闭环）

1. 代码依赖与漏洞扫描：`cargo audit`。
2. 密钥材料静态扫描（私钥/助记词/token）：
   - 扫 `crates/`, `docs/`, `.github/`。
3. 输出 `artifacts/security/security-audit-<date>.md`，至少包含：
   - 发现项、风险等级、修复状态、owner、deadline。

### E3 密钥托管与轮换演练

1. 明确生产密钥存放位置、访问人、审批流程。
2. 做一次“轮换前后可用性验证”：
   - 轮换前签名成功；
   - 轮换后签名成功；
   - 旧密钥撤销成功。
3. 按 `docs/KEY_MANAGEMENT_ACCEPTANCE.md` 逐项留证，至少覆盖：
   - KMS/HSM 托管形态
   - RBAC/IAM 与双人审批
   - 轮换窗口与紧急吊销
   - 审计日志与告警
4. 输出 `artifacts/security/key-rotation-drill-<date>.md`。

### F1-F4 可观测与回滚

1. 启动 OTel + metrics，确认关键指标：
   - 请求量、错误率、延迟、share 接收率、节点高度推进。
2. 配置并触发告警（服务不可用、错误率突增、DB 异常）。
3. 按 runbook 执行一次回滚并记录恢复时间（RTO）。
4. 输出：
   - `artifacts/ops/observability-drill-<date>.md`
   - `artifacts/ops/rollback-drill-<date>.md`

### G1-G3 压测与长稳

1. 压测：按 [运行手册](book/70-operations/runbook.md) / [性能基准](book/70-operations/benchmarks.md) 和 [`scripts/perf_compute_tps.sh`](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/scripts/perf_compute_tps.sh) 执行，记录 TPS、P95/P99、CPU、内存。
2. 长稳：至少 24h，记录异常、重启次数、内存趋势。
3. 峰值故障注入：验证是否可自动恢复。
4. 输出 `artifacts/perf/perf-soak-<date>.md`。

---

## 4) 问题与解决（实时记录）

| 时间(UTC) | 组件 | 问题 | 影响 | 解决方案 | 状态 |
|---|---|---|---|---|---|
| 2026-03-07 | rabbitchain-explorer backend | `/api/accounts/0x...` 返回 400 | 浏览器地址详情失败 | 接口统一接受 `0x`/`0x` 解析（`ef99457`） | Closed |
| 2026-03-07 | rabbitchain-wallet-mobile | 转账页仅接受 `0x...`，易误导粘贴 `0x...` | 用户可能误操作 | 增加地址类型识别、混用二次确认、链 ID 强校验 | Closed |

---

## 5) Go/No-Go 门槛

- 满足上述所有阻塞项并附证据后，才能将结论从 `NO-GO` 改为 `GO`。
- 若出现 `WAIVED`，必须附：
  - 风险说明
  - 临时缓解措施
  - 负责人
  - 明确截止日期
