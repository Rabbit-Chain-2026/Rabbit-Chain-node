# Mainnet Remote Bring-up

适用范围：

- 远端受控 `bootnode`（首节点）
- 本地 `follower`（同步节点）/ `observer`（只读节点）
- 远端受控启网前预检

## 1. 先做预检

```bash
cd Rabbit-Chain-node
bash scripts/mainnet_remote_preflight.sh
```

预检项：

- SSH 可达
- 远端 `rabbitchain` 二进制存在
- 远端工作目录可用
- 远端 RPC / P2P 端口空闲

## 2. 受控 bring-up

```bash
cd Rabbit-Chain-node
bash scripts/mainnet_remote_cycle.sh
```

该入口会顺序执行：

1. `scripts/mainnet_remote_preflight.sh`
2. `scripts/public_node_reset_and_verify.sh`
3. `scripts/mainnet_checklist.sh`

## 3. 当前说明

如果远端 SSH 不可达：

- `mainnet_remote_preflight.sh` 会直接失败
- 不应继续执行真正的 bring-up

也就是说：

- 本地预演链路已可独立闭环
- 远端受控启网链路当前取决于远端 SSH 与主机可达性
