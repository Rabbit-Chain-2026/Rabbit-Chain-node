# 节点同步检查清单

适用范围：
- 本地主挖矿节点（默认 `127.0.0.1:19545`）
- 本地公网节点（默认 `127.0.0.1:29645`）
- 远端公网节点（默认 `139.180.207.66:28545`，通过 SSH 访问）

## 固定检查指标（每次都检查）

1. 进程与 RPC 可用性
- 主挖矿节点 RPC 可达。
- 本地公网节点 RPC 可达。
- 远端公网节点 RPC 可达。

2. 网络参数一致性
- 公网双节点 `net_version` 一致，且等于预期值（当前 `10086`）。

3. 对等连接健康
- 公网双节点 `net_peerCount >= 1`。
- 本地公网节点 `rabbit_peers` 包含远端端点（当前默认 `139.180.207.66:30303`）。

4. 区块同步一致性
- 公网双节点最新区块高度差 `<= MAX_PUBLIC_BLOCK_GAP`（默认 `0`）。
- 公网双节点最新区块高度 `>= MIN_PUBLIC_BLOCK_HEIGHT`（默认 `0`）。

5. 长稳监控状态（soak monitor）
- `LOCAL_OK=1`、`REMOTE_OK=1`。
- `LOCAL_NODE_ALIVE=1`、`REMOTE_NODE_ALIVE=1`。
- 错误计数均为 0：
  - `LOCAL_DROP_EVENTS`
  - `REMOTE_DROP_EVENTS`
  - `LOCAL_RPC_ERRORS`
  - `REMOTE_RPC_ERRORS`
  - `SSH_ERRORS`

## 一键执行

```bash
scripts/node_sync_check.sh
```

返回约定：
- Exit Code `0`：全部指标通过
- Exit Code `1`：存在失败项（输出中会标明 `[FAIL]`）

## 可调参数（环境变量）

```bash
EXPECTED_NET_VERSION=10086 \
MIN_PUBLIC_PEERS=1 \
MAX_PUBLIC_BLOCK_GAP=0 \
MIN_PUBLIC_BLOCK_HEIGHT=0 \
REMOTE_HOST=139.180.207.66 \
SSH_KEY=/root/.ssh/agent_139_180_207_66 \
scripts/node_sync_check.sh
```

## 标准重置测试流程（推荐）

当需要“修复后清空数据并重启验证”时，统一使用：

```bash
scripts/public_node_reset_and_verify.sh
```

该脚本固定执行：
- 停止公网监控、本地公网节点、远端公网节点。
- 清空本地与远端公网节点数据目录。
- 重启远端公网节点。
- 重启本地公网节点（开启 `--mine`）。
- 重新启动公网 soak 监控。
- 在超时时间内反复采样，直到满足：
  - `local peers >= 1 && remote peers >= 1`
  - `local/remote block >= VERIFY_MIN_HEIGHT`（默认 `1`）
  - `|local_block - remote_block| <= VERIFY_MAX_GAP`（默认 `8`）

常用参数示例：

```bash
VERIFY_TIMEOUT_SECS=240 \
VERIFY_MIN_HEIGHT=1 \
VERIFY_MAX_GAP=8 \
REMOTE_HOST=139.180.207.66 \
SSH_KEY=/root/.ssh/agent_139_180_207_66 \
scripts/public_node_reset_and_verify.sh
```
