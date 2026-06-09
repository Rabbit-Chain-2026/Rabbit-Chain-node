# Full-Chain E2E

## 目标

验证节点、矿池、矿工、浏览器在 RabbitChain 接口下可联通运行。

## 检查项

- [x] 组件健康检查通过
- [x] `rabbit_getLatestBlock` 区块高度增长
- [x] 矿池 shares 增长
- [x] `rabbit_getAccount` 返回规范 `0x` 地址
- [x] Explorer 地址查询与搜索可用

## 关键 RPC

- `rabbit_getLatestBlock`
- `rabbit_getAccount`
- `rabbit_getWork`
- `rabbit_submitWork`

## 备注

该文档仅保留当前主路径结果；已移除的旧 transfer RPC 不再纳入检查项。
