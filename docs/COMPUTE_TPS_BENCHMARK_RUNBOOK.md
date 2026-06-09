# Compute TPS Benchmark Runbook

本文定义 RabbitChain compute 性能基准测试和真实交易测试的标准流程。

以后做压测，默认先走这里的流程和 [`scripts/perf_compute_tps.sh`](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/scripts/perf_compute_tps.sh)。

## 目标

这份 runbook 只覆盖 compute TPS 相关压测：

- 本地标准压测，用来比较代码变更前后是否退化
- 真实交易测试，用来验证线上或准线上入口吞吐

不建议把这份流程和通用 `cargo bench` 混用。通用 benchmark 只适合库级微基准，不适合这里的 RPC / compute 压测口径。

## 标准入口

本地标准压测：

```bash
bash scripts/perf_compute_tps.sh
```

真实交易测试：

```bash
RABBIT_TPS_RPC_URL=https://example-rpc \
RABBIT_TPS_RPC_TOKEN=your_token \
bash scripts/perf_compute_tps.sh submit-benchmark
```

多节点真实交易测试：

```bash
RABBIT_TPS_RPC_URLS=http://127.0.0.1:18545,http://127.0.0.1:29645,http://127.0.0.1:39745 \
RABBIT_TPS_RPC_TOKEN=your_token \
RABBIT_TPS_INGRESS_CONCURRENCY=768 \
bash scripts/perf_compute_tps.sh submit-benchmark
```

## 真实交易测试怎么跑

`submit-benchmark` 这个 mode 名称表示“打到一个真实可写的 RPC 节点”，不限定这个节点必须在远端。它可以是：

- 你自己在本机起的受控节点
- staging / canary / 预发布环境里的节点

如果只是为了记录一版可复现的结果，推荐先起本地受控节点，再跑 `submit-benchmark`。

### 推荐：本地受控节点

最小节点只需要 `bootnode`。示例：

```bash
export RPC_AUTH_TOKEN=perf-benchmark-token

bash scripts/mainnet.sh start bootnode \
  --mine \
  --disable-local-miner \
  --coinbase 0x526Dc404e751C7d52F6fFF75d563d8D0857C94E9 \
  --rpc-auth-token "$RPC_AUTH_TOKEN" \
  --rpc-rate-limit-per-minute 0 \
  --p2p-listen-addr 127.0.0.1
```

然后把 benchmark 指到这个本地节点：

```bash
export RABBIT_TPS_RPC_URL=http://127.0.0.1:8545
export RABBIT_TPS_RPC_TOKEN="$RPC_AUTH_TOKEN"
bash scripts/perf_compute_tps.sh submit-benchmark
```

如果是多节点横向压测，把多个 RPC 地址用逗号拼到 `RABBIT_TPS_RPC_URLS`，脚本会按 tx index 轮询分流到各节点。

最小可用口径就是这两个值：

- `RABBIT_TPS_RPC_URL=http://127.0.0.1:8545`
- `RABBIT_TPS_RPC_TOKEN=$RPC_AUTH_TOKEN`

如果你想一并拉起 follower / observer / pool / miner / explorer 的完整本地拓扑，可以直接用：

```bash
export RPC_AUTH_TOKEN=perf-benchmark-token
bash scripts/mainnet_local_bringup.sh
```

这时再把 `RABBIT_TPS_RPC_URL` 和 `RABBIT_TPS_RPC_TOKEN` 指向它打印出来的：

- `bootnode RPC`: `http://127.0.0.1:8545`
- `RPC_AUTH_TOKEN`: 你导出的那个值

完整拓扑适合联调，最小 bootnode 适合单纯记录真实交易测试结果。

## 标准档位

### 本地标准档

推荐参数：

| 项目 | 默认值 | 说明 |
|---|---:|---|
| `RABBIT_TPS_TX_COUNT` | `1000000` | 单次样本量 |
| `RABBIT_TPS_INGRESS_CONCURRENCY` | `128` | RPC ingress 并发 |
| `RABBIT_TPS_DIRECT_FLUSH_EVERY` | `1000000` | direct 路径只在末尾 flush |
| `RABBIT_TPS_PERSIST_BATCH_SIZE` | `128` | persistence 批量写大小 |

对应测试入口是：

```bash
cargo test -p rabbitapi --test compute_tps_bench compute_tps_benchmark -- --ignored --nocapture
```

### 真实交易标准档

推荐参数：

| 项目 | 默认值 | 说明 |
|---|---:|---|
| `RABBIT_TPS_TX_COUNT` | `1000000` | 单次样本量 |
| `RABBIT_TPS_INGRESS_CONCURRENCY` | `256` | 请求并发 |

多节点 sweep 时，建议把 `RABBIT_TPS_INGRESS_CONCURRENCY` 设成 `256 * n` 到 `512 * n`，其中 `n` 是参与分流的 RPC 节点数。

对应测试入口是：

```bash
cargo test -p rabbitapi --test compute_tps_bench compute_tps_submit_benchmark -- --ignored --nocapture
```

## 前置条件

### 本地标准压测

- 仓库可正常构建
- `cargo test -p rabbitapi --test compute_tps_bench` 能通过
- 机器上不要同时跑其他重负载任务

### 真实交易测试

- 使用的是专门的 staging、canary，或者已经批准的压测窗口
- 或者使用你自己在本机起的受控节点
- 目标 RPC 节点有明确的 token
- 不要把这类测试直接打到未批准的生产入口
- 压测前后都要记录 commit hash 和运行参数

## 执行步骤

### 1. 选择模式

- 本地标准压测：默认模式
- 真实交易测试：`submit-benchmark`

### 2. 运行标准脚本

本地标准压测：

```bash
bash scripts/perf_compute_tps.sh
```

真实交易测试：

```bash
RABBIT_TPS_RPC_URL=https://example-rpc \
RABBIT_TPS_RPC_TOKEN=your_token \
bash scripts/perf_compute_tps.sh submit-benchmark
```

如果是本机受控节点，先起节点，再把 `RABBIT_TPS_RPC_URL` / `RABBIT_TPS_RPC_TOKEN` 指到 `http://127.0.0.1:8545` 和对应 token。

### 3. 采集结果

脚本会自动写入：

- `artifacts/perf/compute-tps/<mode>/<timestamp>/run.log`
- `artifacts/perf/compute-tps/<mode>/<timestamp>/meta.txt`
- `artifacts/perf/compute-tps/<mode>/<timestamp>/report.md`

`run.log` 是原始输出，`meta.txt` 记录这次运行的固定参数和 git 版本，`report.md` 记录这次运行是否成功、耗时和命令行。

### 4. 记录辅助指标

TPS 不是唯一指标。正式测试报告至少还要补下面这些数据：

- CPU 使用率
- 内存占用
- RPC 错误率
- `rabbit_getMetrics` 快照
- 如果是真实 RPC，还要记录目标 URL 的环境类型
- 如果是真实交易测试，还要记录节点是本地受控节点还是远端节点

## 标准报告口径

建议固定使用同一组参数，至少重复 3 次，再取中位数作为最终结果。

每次运行都必须保留记录，不能只看终端输出。

如果发现一次结果明显偏低，不要立刻下结论，先检查：

- 是否有其他任务抢占 CPU
- 是否是热编译导致的第一次慢
- `RABBIT_TPS_TX_COUNT` 是否太小，导致噪声占比过高
- 真实 RPC 时是否有网络抖动或 token 限流
- 真实 RPC 时是否把 `RABBIT_TPS_INGRESS_CONCURRENCY` 设得太低；这条口径是同步等待提交结果的，`64` 往往会明显低估吞吐

## 结果模板

建议每次压测都保留下面这些字段：

```text
mode=
commit=
tx_count=
ingress_concurrency=
direct_flush_every=
persist_batch_size=
rpc_url=
elapsed=
tps=
cpu=
memory=
error_rate=
notes=
```

## 以后怎么做

以后只要是 compute TPS 压测，优先使用：

1. [`scripts/perf_compute_tps.sh`](/home/de/works/RabbitChain-workspaces/Rabbit-Chain-node/scripts/perf_compute_tps.sh)
2. 本文档
3. `artifacts/perf/compute-tps/...` 下的原始日志

不要再临时拼 `cargo test` 命令当成标准压测流程。
