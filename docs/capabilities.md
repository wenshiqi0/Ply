# 当前系统能力与验收范围

Wendy 的当前范围是 macOS 上的 Rust/JSC agent harness：CLI、脚本和 MCP 共用同一个内核，TS 插件提供工具与上下文，DeepSeek/MiMo 通过 JSC 内嵌 pi-ai 和 Rust HTTP 调用。下面按系统行为列出验收路径，不把模型评审报告的正确率当作系统验收。

| 能力 | 已实现的完整路径 | 验收来源 |
|---|---|---|
| 使用入口 | tools 描述插件；call 返回输出/错误；run 运行脚本；MCP 提供工具服务；commands 列出注册入口；插件可注册顶层命令，tui 启动面板 | [CLI 端到端](../crates/wendy-cli/tests/entrypoints.rs)、[真实 PTY](../crates/wendy-cli/tests/tui.rs) |
| 插件与命令模块 | wendy:plugins 显式导入实时目录，移除旧全局 tools；wendy:commands 只在 onLoad 注册，原生 realm 归属、重复拒绝/失败撤销、独立 fiber 与取消清理 | [模块与命令](commands.md)、[内核验收](../crates/wendy-kernel/tests/commands.rs) |
| MCP 请求处理 | 单行 JSON-RPC 并发处理；非法 JSON、版本/方法/id、参数对象返回明确错误；合法通知和入站响应不回包；坏请求后继续服务 | [CLI 端到端](../crates/wendy-cli/tests/entrypoints.rs)、[入口实现](../crates/wendy-cli/src/main.rs) |
| 插件事件 | onLoad 声明/订阅命名空间事件；运行时异步分发，监听器独立子 fiber；异常/超时/取消阻止 bash 执行；失败加载回收注册 | [事件实现](../crates/wendy-kernel/src/events.rs)、[hook 集成](../crates/wendy-kernel/tests/kernel.rs) |
| 插件与 ctx | TS 签名转工具定义、onLoad 等待与发布、共享闭包、ctx 方法挂载、重复/加载失败处理、临时 realm 回收 | [内核集成](../crates/wendy-kernel/tests/kernel.rs)、[签名实现](../crates/wendy-kernel/src/signature.rs) |
| 文件和进程 | read 分页、create/edit/remove、只读挂载拒写、内部 Wendy sh 执行与超时/取消；磁盘/内存/SQLite 后端行为一致 | [入口测试](../crates/wendy-cli/tests/entrypoints.rs)、[存储一致性](../crates/wendy-storage/tests/conformance.rs)、[IO](../crates/wendy-io/tests/io_layer.rs) |
| fiber 调度与归属 | 共享 Promise 按消费者归属；并发父子关系、ctx/tools/timer/续体保持调用身份；每个 Spawn 有唯一 Exit | [JSC 集成](../crates/wendy-jsc/tests/node_outlet.rs)、[内核集成](../crates/wendy-kernel/tests/kernel.rs) |
| 错误与取消 | 普通错误、rejection、定时器错误、宿主 panic、CPU/内存超限有明确终态；父取消传播到子，独立兄弟继续；同步 CPU/exec 等待响应取消 | [JSC 集成](../crates/wendy-jsc/tests/node_outlet.rs)、[内核取消](../crates/wendy-kernel/tests/kernel.rs) |
| 清理生命周期 | 失败/中断后排队执行 onDispose，整批两秒截止；回调异常继续后续回调，超时停止；清理完成也回收未 await 的定时器/宿主任务；独立 Cleanup 记录 owner 与终态 | [清理集成](../crates/wendy-jsc/tests/node_outlet.rs) |
| 环境审批协调 | approval.request 保持 bash 协议，toolRequest 覆盖模型工具调用；读取前审批，决定与原参数绑定；TUI 只调用 agent 并展示审批；默认主/子 agent 分别六/五个工具；无环境返回 approval_required | [审批协议](approval.md)、[模型工具门禁](../crates/wendy-kernel/tests/approval.rs)、[真实 PTY](../crates/wendy-cli/tests/tui.rs) |
| Jev 安全检查 | 可选订阅 bash.beforeExecute，原生 state/questions/Noul，阈值建议、结构化结果、异常或不确定转审批；私有密钥及插件模型用量归属 | [Jev 说明](jev.md)、[原生 API 集成](../crates/wendy-kernel/tests/jev.rs) |
| agent 委派 | 独立对话、工具白名单、子级只能收窄、代数限制；累计直属子数在内核原子准入，失败/完成/替换/恢复不退款；模型轮数默认不限 | [agent 集成](../crates/wendy-kernel/tests/agent.rs)、[配额并发](../crates/wendy-kernel/tests/kernel.rs) |
| 报告完成 | 正常停止、非空、结束标记、围栏、协议残留、要求章节；失败由原 agent 修复，截断工具参数不执行 | [agent 集成及模型 fixture](../crates/wendy-kernel/tests/agent.rs) |
| 上下文 | 总窗口/输出预算/模型上限；估算与 usage 锚点；完整工具批次切分；版本化摘要提交、失败重试与冷却、硬预算保护 | [context 集成](../crates/wendy-kernel/tests/context.rs)、[agent 集成](../crates/wendy-kernel/tests/agent.rs) |
| 失败恢复 | 私有原子快照、排他恢复、格式 2 拒绝旧版本；恢复对话、权限、累计配额、报告状态及历史用量；tools_pending 禁止自动重放 | [agent 恢复用例](../crates/wendy-kernel/tests/agent.rs)、[宿主快照](../crates/wendy-jsc/src/checkpoints.rs) |
| 模型运行时 | 内嵌 DeepSeek/MiMo 接入、SSE 流读取、reasoning/tool/usage 透传、并发隔离、取消、体积上限；密钥只从宿主取，模型调用无需外部 Node | [SDK/本地 HTTP 集成](../crates/wendy-jsc/tests/model_runtime.rs)、[HTTP 实现](../crates/wendy-jsc/src/http.rs)、[模型选择](../crates/wendy-cli/src/main.rs) |
| 用量与事件 | model_call_started/end 配对、自身/子树汇总、缺失用量标记、按 token 加权缓存命中率；JSONL 写 Spawn/Record/Exit/Cleanup/Plugin | [agent 集成](../crates/wendy-kernel/tests/agent.rs)、[事件实现](../crates/wendy-core/src/fiber.rs) |

## 验收命令

```bash
cargo test --workspace --locked --offline
cargo build -p wendy-cli --locked --offline
```

测试中的模型 fixture 和本地 HTTP 服务不产生供应商调用，它们验证系统行为与协议处理。真实 DeepSeek/MiMo 的供应商连通性、用量和进程审计应另留运行轨迹；本地测试通过不能代替供应商实测，也不代表所有 provider 已接入。主 agent 开放 API key 认证的 DeepSeek 和 MiMo；Jev 作为独立的原生决策 API 订阅插件接入。

MCP 的请求字段与错误码依据 [JSON-RPC 2.0](https://www.jsonrpc.org/specification)；这里只列出 Wendy 已实现的 stdio 工具服务，不声称实现全部 MCP 扩展。

## 行为边界

- 插件代码是可信执行边界；模型 bash tool 的文件访问通过内部 sh 和挂载 IO；其他插件直接使用的宿主子进程不是 OS 沙箱。
- 同插件共享闭包与一个 JS 线程；错误隔离不回滚已发生的共享状态或 IO。内存阈值是进程水位。
- onDispose 服务于失败/中断，正常调用用 finally；必须完成的清理需要 await。清理批次结束后，未等待的后台工作会取消，不能依靠它继续运行。
- 恢复保留安全交互点；首次强制 VM 中断不保证生成新快照，已有 tools_pending 快照不自动重放。旧快照格式明确拒绝。
- 上下文 token 是保护性估算；缓存读取仍占上下文。报告完整性校验只验证格式和完成状态，不验证模型结论真伪。
