# RabbitChain Compute TPS 公开测试报告

本文记录一次面向公众可复现的 RabbitChain compute TPS 测试，覆盖单节点真实交易测试和同机多节点 round-robin 真实交易测试。

## 结论摘要

在同一台机器上把 3 个 RPC 入口通过 `RABBIT_TPS_RPC_URLS` 做 round-robin，并没有带来线性扩展。单入口 `256` 并发时的吞吐仍然是这台机器上的最高点；当入口扩展到 3 个、并发提高到 `768` 和 `1536` 后，TPS 反而下降，说明瓶颈主要来自同机资源争用和 RPC / 执行协调开销，而不是入口数量本身。

## 测试范围

- 测试类型：真实交易测试
- 入口脚本：[`scripts/perf_compute_tps.sh`](../scripts/perf_compute_tps.sh)
- 测试目标：`compute_tps_submit_benchmark`
- 数据样本：`1,000,000` 笔交易
- 构建模式：`release`
- 代码版本：`d92d298`
- 分流方式：按交易序号对多个 RPC 地址做 round-robin

## 测试环境

这组结果来自一台本地机器上的三个 RPC 入口：

- `http://127.0.0.1:18545`
- `http://127.0.0.1:29645`
- `http://127.0.0.1:39745`

这不是跨机器的分布式集群测试，而是“同机多进程 + 多入口分流”的 LB-style benchmark。它适合回答“入口层能顶到多少”，不适合直接当作“分布式横向扩展上限”。

## 测试方法

### 单节点真实交易基线

```bash
export RABBIT_TPS_RPC_URL=http://127.0.0.1:18545
export RABBIT_TPS_RPC_TOKEN=perf-benchmark-token
RABBIT_TPS_TX_COUNT=1000000 \
RABBIT_TPS_INGRESS_CONCURRENCY=256 \
bash scripts/perf_compute_tps.sh submit-benchmark
```

### 三入口 round-robin 测试

```bash
export RABBIT_TPS_RPC_URLS=http://127.0.0.1:18545,http://127.0.0.1:29645,http://127.0.0.1:39745
export RABBIT_TPS_RPC_TOKEN=perf-benchmark-token
RABBIT_TPS_TX_COUNT=1000000 \
RABBIT_TPS_INGRESS_CONCURRENCY=768 \
bash scripts/perf_compute_tps.sh submit-benchmark
```

```bash
export RABBIT_TPS_RPC_URLS=http://127.0.0.1:18545,http://127.0.0.1:29645,http://127.0.0.1:39745
export RABBIT_TPS_RPC_TOKEN=perf-benchmark-token
RABBIT_TPS_TX_COUNT=1000000 \
RABBIT_TPS_INGRESS_CONCURRENCY=1536 \
bash scripts/perf_compute_tps.sh submit-benchmark
```

## 结果

| 场景 | RPC 入口数 | 并发 | 耗时 | TPS | 备注 |
|---|---:|---:|---:|---:|---|
| 单节点基线 | 1 | 256 | 42.64467078s | 23449.59 | 这台机器上的最高观测点 |
| 3 入口 round-robin | 3 | 768 | 47.392332594s | 21100.46 | `256 * 3` |
| 3 入口 round-robin | 3 | 1536 | 48.763693714s | 20507.06 | `512 * 3` |

## 解释

- 单节点 `256` 并发时的吞吐最高，说明真实交易路径在这台机器上有一个比较明确的甜点区。
- 当入口数增加到 3、并发继续抬升后，TPS 没有继续增长，反而略微下降。
- 这说明当前瓶颈不是“入口不够多”，而是同机资源争用、调度和提交协调开销。
- 如果要证明真正的横向扩展，必须把节点拆到不同机器上再测。

## PR 里可以直接贴的结论

在同一台机器上把 3 个 RPC 入口通过 `RABBIT_TPS_RPC_URLS` 做 round-robin，并没有带来线性扩展：单入口 `256` 并发时达到 `23,449.59 TPS`，3 入口 `768` 并发时为 `21,100.46 TPS`，`1,536` 并发时为 `20,507.06 TPS`。这说明当前瓶颈主要来自同机资源争用和 RPC / 执行协调开销，而不是入口数量本身；如果要验证真正的横向扩展，需要把 RPC 节点拆到不同机器上再测。

## 记录文件

每次运行都自动写入以下文件：

- `artifacts/perf/compute-tps/submit-benchmark/20260602T203521Z/run.log`
- `artifacts/perf/compute-tps/submit-benchmark/20260602T203521Z/meta.txt`
- `artifacts/perf/compute-tps/submit-benchmark/20260602T203521Z/report.md`

- `artifacts/perf/compute-tps/submit-benchmark/20260602T211647Z/run.log`
- `artifacts/perf/compute-tps/submit-benchmark/20260602T211647Z/meta.txt`
- `artifacts/perf/compute-tps/submit-benchmark/20260602T211647Z/report.md`

- `artifacts/perf/compute-tps/submit-benchmark/20260602T211754Z/run.log`
- `artifacts/perf/compute-tps/submit-benchmark/20260602T211754Z/meta.txt`
- `artifacts/perf/compute-tps/submit-benchmark/20260602T211754Z/report.md`

## 适用范围

这份报告只回答一个问题：在当前实现和当前机器上，真实交易入口能跑到什么量级。

它不代表：

- 跨机器分布式集群的最终上限
- 生产环境的绝对吞吐
- 长时间稳定性或 SLA

如果要做发布门槛，建议在同一组固定参数下重复 3 次，然后取中位数。
