# Rabbit-Chain-node Repo Rules

## Redline First

- 默认 `fail-fast`：配置、存储、同步、协议错误必须显式失败。
- 禁止 silent fallback：不得回退到 `default` / `mem` 继续运行。
- 禁止在关键链路吞错：不要以 warning 代替失败返回。
- 兼容逻辑必须显式开关，默认关闭，且需评审批准。
- 允许例外时，必须添加 `REDLINE_ALLOW` + 原因注释。

## Required Validation

改动涉及核心链路时，至少执行：

```bash
bash scripts/no_silent_fallback.sh
cargo test -p rabbitnet -p rabbitapi
cargo check -p rabbitcli
```
