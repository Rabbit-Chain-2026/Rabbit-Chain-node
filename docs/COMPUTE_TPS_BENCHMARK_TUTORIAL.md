# RabbitChain Compute TPS 公开测试教程

本文面向外部读者，说明如何复现 RabbitChain 的 compute TPS 测试，如何记录结果，以及如何区分“本地性能基准”和“真实交易测试”。

如果你只想看最终结果，直接跳到 [`COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md`](./COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md)。

## 这份教程覆盖什么

- 本地性能基准：验证代码变更前后是否退化
- 真实交易测试：验证真实 RPC 入口吞吐
- 单节点和多节点 round-robin 两种真实交易口径
- 每次测试都自动留档，避免“只看终端输出”

## 先决条件

- 仓库可以正常构建
- 已经准备好一个本地受控节点，或者一个已批准的 staging / canary / 预发布 RPC
- 你有可用的 RPC token
- 机器上没有其他重负载任务抢占 CPU

## 标准入口

本地性能基准：

```bash
bash scripts/perf_compute_tps.sh
```

真实交易测试：

```bash
RABBIT_TPS_RPC_URL=https://example-rpc \
RABBIT_TPS_RPC_TOKEN=your_token \
bash scripts/perf_compute_tps.sh submit-benchmark
```

真实挖矿 e2e：

```bash
bash scripts/mining_e2e.sh
```

多节点 round-robin 真实交易测试：

```bash
RABBIT_TPS_RPC_URLS=http://127.0.0.1:18545,http://127.0.0.1:29645,http://127.0.0.1:39745 \
RABBIT_TPS_RPC_TOKEN=your_token \
RABBIT_TPS_TX_COUNT=1000000 \
RABBIT_TPS_INGRESS_CONCURRENCY=768 \
bash scripts/perf_compute_tps.sh submit-benchmark
```

`RABBIT_TPS_RPC_URLS` 是逗号分隔的多个 RPC 地址。脚本会按交易序号做 round-robin 分流。

## 推荐跑法

### 1. 本地性能基准

这个模式只看执行和提交路径，不依赖外部 RPC 环境。

```bash
bash scripts/perf_compute_tps.sh
```

默认参数是：

- `RABBIT_TPS_TX_COUNT=1000000`
- `RABBIT_TPS_INGRESS_CONCURRENCY=128`
- `RABBIT_TPS_DIRECT_FLUSH_EVERY=1000000`
- `RABBIT_TPS_PERSIST_BATCH_SIZE=128`

### 2. 单节点真实交易测试

先起一个本地受控节点，再跑真实交易测试。

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

然后把 benchmark 指向这个节点：

```bash
export RABBIT_TPS_RPC_URL=http://127.0.0.1:8545
export RABBIT_TPS_RPC_TOKEN="$RPC_AUTH_TOKEN"
bash scripts/perf_compute_tps.sh submit-benchmark
```

### 3. 多节点 round-robin 真实交易测试

如果你要测入口层吞吐，可以把多个本地 RPC 入口都喂给 benchmark。

```bash
export RABBIT_TPS_RPC_URLS=http://127.0.0.1:18545,http://127.0.0.1:29645,http://127.0.0.1:39745
export RABBIT_TPS_RPC_TOKEN=perf-benchmark-token
RABBIT_TPS_TX_COUNT=1000000 \
RABBIT_TPS_INGRESS_CONCURRENCY=768 \
bash scripts/perf_compute_tps.sh submit-benchmark
```

建议先扫一轮：

- `256 * n`
- `512 * n`

其中 `n` 是参与分流的 RPC 节点数。

如果你要验证真正的横向扩展，节点最好分布在不同机器上。同一台机器上的多个 RPC 进程只能说明“本地多入口分流”的上限，不等于分布式集群的最终吞吐。

### 4. 真实挖矿 e2e

这个入口会同时记录：

- compute 执行结果
- `rabbit_getReceipt` 的观测值
- 区块高度变化
- pool / miner share 指标

```bash
bash scripts/mining_e2e.sh
```

它适合回答“交易有没有被执行、链是否在出块、矿工是否真的在工作”，不适合拿来做纯 submit TPS 对比。

## 每次测试都必须留档

脚本会自动把结果写到：

- `artifacts/perf/compute-tps/<mode>/<timestamp>/run.log`
- `artifacts/perf/compute-tps/<mode>/<timestamp>/meta.txt`
- `artifacts/perf/compute-tps/<mode>/<timestamp>/report.md`

三个文件缺一不可：

- `run.log` 保存原始输出
- `meta.txt` 保存参数和 git 版本
- `report.md` 保存本次运行是否成功、耗时和命令行

## 如何解读结果

- `submit-benchmark` 是同步等待提交结果的口径，`64` 并发通常会明显低估吞吐
- 如果并发更高反而 TPS 更低，通常是机器共享资源、RPC 排队或执行协调开销在抬升
- 如果你要做正式发布口径，至少重复 3 次，再取中位数

## 相关文档

- 标准操作手册：[`运行手册`](book/70-operations/runbook.md) / [`性能基准`](book/70-operations/benchmarks.md)
- 本次公开测试报告：[`COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md`](./COMPUTE_TPS_BENCHMARK_REPORT_2026-06-02.md)
