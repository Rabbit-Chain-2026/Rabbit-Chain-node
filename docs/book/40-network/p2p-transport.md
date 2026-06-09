---
id: network.p2p_transport
title: P2P 传输
kind: chapter
status: verified
owner: core-docs
primary_topic: network.p2p_transport
topics: []
depends_on:
  - sync.gossip
aliases:
  - p2p.websocket.cdn
evidence:
  - type: code
    ref: scripts/mainnet.sh
  - type: code
    ref: crates/rabbitnet/src/lib.rs
  - type: code
    ref: crates/rabbitnet/src/discovery.rs
  - type: code
    ref: crates/rabbitcli/src/main.rs
  - type: code
    ref: scripts/p2p_three_node_smoke.sh
  - type: doc
    ref: docs/P2P_WEBSOCKET_CDN.md
last_reviewed: 2026-06-05
review_due: 2026-07-05
---

# P2P 传输

这章讲的是对等节点如何连，分哪些传输形式，以及 WebSocket/CDN 方案在当前实现里处于什么位置。

## 范围

- 这章只讲传输与 bootnode 连接形式。
- 这章不展开 gossip 传播算法本身。
- 这章不把 CDN 讲成缓存层，它只是 WebSocket 入口的反向代理路径。

## 传输模式

当前实现支持两种可独立切换的 bootnode 形式：

- `enode://peer@ip:port`，对应 direct TCP。
- `ws://...` / `wss://...`，对应 WebSocket P2P。

代码和脚本里都能看到这两种路径：

- CLI 通过 `--disable-p2p-tcp`、`--disable-p2p-ws`、`--p2p-ws-listen-port`、`--p2p-ws-external-url` 控制。
- `scripts/mainnet.sh` 和 `scripts/p2p_three_node_smoke.sh` 都直接调用这些参数。
- `crates/rabbitnet/src/lib.rs` 和 `crates/rabbitnet/src/discovery.rs` 负责实际连接和解析。

## WebSocket / CDN 路径

WebSocket 传输的目标是把 P2P bootnode 暴露成一个可被 HTTP 反向代理接入的入口。
这条路径适合 Cloudflare orange-cloud 或类似的边缘代理，但它不负责缓存 P2P 流量，也不替代协议级限流。

### 运行方式

- 直接 TCP：默认可用。
- WebSocket 仅监听：显式开启 `--p2p-ws-listen-port`。
- TCP + WebSocket 并存：同时配置两个监听。
- CDN / 反代入口：只对外暴露 HTTPS/WebSocket，origin 端口绑在 localhost 并通过防火墙限制访问。

### 连接策略

- 直连 bootnode 和 WebSocket bootnode 可以同时配置。
- `--disable-discovery` 适合静态 WebSocket bootnode 场景，因为当前 discovery 还是面向 IP/UDP ENR 的 direct TCP 路径。

## 运维约束

- private/sentry peer 仍然建议保留 direct TCP。
- WebSocket bootnode 更适合 boot / sentry 入口，而不是把所有 gossip 都压到单一反代入口。
- 节点侧 P2P 限流必须继续开启。
- 不要相信未经验证的 forwarded headers 来做协议判断。

## 结论

P2P WebSocket/CDN 不是新的共识层，它只是一个可替换的 bootnode 入口。
当前实现里它和 direct TCP 是并列存在的传输选项，而不是互斥的架构重写。
