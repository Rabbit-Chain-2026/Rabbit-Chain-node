# rabbitapi

RabbitChain 的 API 服务层（JSON-RPC / HTTP / WebSocket）。

## 主要内容

- JSON-RPC 路由与实现：`src/rpc/`
- HTTP server：`src/http_server.rs`
- 对外错误类型：`src/error.rs`

该 crate 通常由 `rabbitcli` 的 `rabbitchain` 节点二进制集成并启动。

## 本地开发

在仓库根目录执行：

```bash
cargo test -p rabbitapi
```

