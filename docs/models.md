# 模型接入

agent 在自己的 JSC realm 内运行内嵌的 [pi-ai](https://github.com/earendil-works/pi/tree/main/packages/ai) 模型层，当前接入 DeepSeek 和 MiMo。运行时不需要 Node、npm 或全局安装的 Pi。只用 pi-ai 的模型连接；会话、上下文和压缩都由 Ply 自己管理。

## 传输与密钥

- 模型请求通过 Rust 的 HTTP/TLS 客户端发送，SSE 响应按需逐块读取并交给 pi-ai 解析。
- 每次调用有独立的取消信号和 10 分钟总超时；请求体和累计响应体各限 16 MiB；失败、取消或 realm 退出会释放响应。
- 密钥在请求时从宿主读取，不加入插件的 `process.env`、上下文或用量记录。

## DeepSeek

默认接入，模型为 `deepseek-v4-flash`，密钥在宿主的 `DEEPSEEK_API_KEY`：

```bash
./target/debug/ply call --record fibers.jsonl agent '{"prompt": "把 app.py 的输出改成 hello, ply，然后运行验证"}'
```

## MiMo

用 `--model` 选择 MiMo V2.6，默认使用中国区 Token Plan（`xiaomi-token-plan-cn`，`https://token-plan-cn.xiaomimimo.com/v1`），密钥只从宿主的 `MIMO_API_KEY` 读取：

```bash
./target/debug/ply call --model mimo-v2.6-pro --record mimo-fibers.jsonl agent '{"prompt": "只读总结这个项目，不修改文件"}'
./target/debug/ply call --model mimo-v2.6-flash agent '{"prompt": "只读检查 README 中的命令是否与实现一致"}'
```

`--mimo-provider` 可选 `xiaomi`（按量付费）、`xiaomi-token-plan-cn`、`xiaomi-token-plan-sgp`、`xiaomi-token-plan-ams`。每种服务都从 `MIMO_API_KEY` 取密钥，但按量密钥与 Token Plan 密钥不能互换。`mimo-v2.6-pro-ultraspeed` 仅在 `--mimo-provider xiaomi` 下可用。

```bash
./target/debug/ply call --model mimo-v2.6-pro --mimo-provider xiaomi agent '{"prompt": "只读总结这个项目"}'
```

- MiMo 默认开启思考，历史思考内容随工具调用一起透传；token、推理量和缓存命中率沿用 agent 的统一统计。
- Token Plan 的 SDK cost 元数据是占位值，不能解读为免费或套餐扣减金额，Ply 不把它输出为费用。
- 型号与协议依据 [官方型号说明](https://mimo.mi.com/docs/en-US/quick-start/model)、[API 文档](https://mimo.mi.com/docs/en-US/api/chat) 和 [套餐支持说明](https://mimo.mi.com/docs/en-US/quick-start/faq/token-plan/Plans%26Pricing)，核对于 2026-10-24；V2.5 模型 ID 已退役。
- `--no-default-plugins` 下可用 `--plugin ./plugins/mimo` 手动加载，不要同时加载 deepseek 和 mimo。

## 更新内嵌的 pi-ai

pi-ai bundle 随源码保存，普通 `cargo build` 不运行 Node。只有更新 SDK 或 bundle 时才需要构建期 Node：

```bash
cd packages/pi-ai
npm ci --ignore-scripts --registry=https://registry.npmjs.org
npm run build
```

pi-ai 固定为 `0.82.1`，依赖锁在 `packages/pi-ai/package-lock.json`，生成文件及第三方许可声明在 `crates/ply-jsc/src/vendor/`。OAuth、Bedrock 和其他 provider 尚未接入。

把外部代码改为 npm 依赖、运行时只提供资源能力的调整见 [RFC 0013](rfcs/0013-resources-and-npm.md)（提议中）。
