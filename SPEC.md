# Ply SPEC

状态：当前规范。本文定义系统必须维持的契约；RFC 记录决策、动机与变更过程。

## 基于规范开发

1. 开发前确认本规范中的行为、类型、权限与验收来源。
2. 改变行为或公共接口先提出 RFC，明确兼容范围和验证方式；确认后实现。
3. 实现、SDK 类型、测试与 SPEC 同步修改。已实现 RFC 的有效规则同步回本文；不能只留下 RFC。
4. 内部重构保留已确认的 CLI、插件参数、`ply:plugins` 接口与安全边界。
5. 自动检查通过后记录验证结果。fixture 测试不能代替真实供应商或交互验收。

RFC 索引见 [rfcs/README.md](rfcs/README.md)。插件模块化和类型治理见 [RFC 0011](rfcs/0011-plugin-modules-and-types.md)；插件只见能力、fiber 只在内核见 [RFC 0012](rfcs/0012-capabilities-not-scheduling.md)。

## SPEC-PLUGINS：插件与模块

- 一个目录是一个插件，入口按 `main.ts`、`index.ts`、`main.js`、`index.js` 选择。目录名是插件名。
- 入口具名导出 `onHandle(input)`、`onLoad(options)`、`onDispose()` 中的任意几个；`onHandle` 的 JSDoc、参数、返回类型生成描述与工具 schema。
- `onLoad` 只运行一次，返回的数据与方法通过插件句柄提供；方法是所属 realm 内的异步 RPC。`onDispose` 在 realm 回收时运行一次。
- 插件只见能力，不见调度：运行时不提供 `fiber`、`ctx` 全局和 `ply:lifecycle`，句柄没有 `.loaded`；插件之间通过输入、返回值和持有的值（如会话 id）建立关系，不按调用方判定归属。
- 入口可用插件目录内的相对 import 拆分模块。同一插件的模块共享一个 realm 和模块缓存，不能因拆文件改变 fiber 归属、状态或加载时序。当前导入采用复制绑定；跨模块更新使用稳定对象或方法访问，不依赖可变标量的 live binding。
- 内置插件在二进制中嵌入完整模块目录，普通构建和运行不依赖外部 Node；目录插件与内置插件使用同一模块解析规则。
- 运行时边界：Ply 只提供资源能力（进程环境、进程执行、文件、网络、终端）与内核能力（`ply:plugins`、`ply:events`、`ply:commands`）。外部代码依赖经 node 模块解析从 node_modules 引入，开发模式直接读取 node_modules，打包时随应用嵌入；存储由插件在 fs 上实现，跨进程互斥使用 fs 的文件锁扩展（OS 排他咨询锁，跨进程有效，关闭句柄或进程退出时释放）；密钥通过环境变量注入。现有 `ply:models`、`ply:memory-store`、`ply:checkpoints`、`ply:http`、`ply:skills`、`ply:markdown` 按 [RFC 0013](rfcs/0013-resources-and-npm.md) 迁出，迁移完成前按各自现行契约运行。
- 签名读取支持插件目录内相对文件中的具名类型导入与别名；递归类型保守收敛为 unknown。未支持的 TypeScript 高级类型不得声称具备运行时参数校验。
- `main.ts` 保留入口与装配，`types.ts` 声明输入、输出、onLoad 参数和公共导出；较复杂的策略、存储、渲染、统计等放入职责模块。
- 每个 TS/声明文件最多 250 个非空、非注释行；不得通过合并多条语句或全局关闭检查规避约束。
- 具名句柄由 `import { context, agent, memory } from "ply:plugins"` 引入；动态目录由默认 import 引入。插件的 onLoad 在它具名导入的插件完成 onLoad 后才开始；被导入的插件加载失败时依赖方加载失败，相互导入成环报错；默认导入不建立顺序。对句柄的调用和方法调用等待目标加载完成；`events.on` 等待事件被声明或所属插件 onLoad 结束。
- `ply:commands` 在 onLoad 注册宿主入口，不扩大模型工具权限。默认工具由 context 收集，agent 按授权收窄。
- Node 风格 API 由 JSC shim 提供；编辑器的 Node 类型不意味着支持完整 Node 运行时。支持范围以运行时及其测试为准。
- Ply 运行时以 Node 为基线，可以作为 Node 的超集：已提供的 Node API 遵循 Node 语义；Node 缺少的资源能力（如文件锁）作为扩展加在对应的 Node 对象上，不另起 `ply:` 模块。

## SPEC-TYPES：类型与静态检查

- 根 `tsconfig.json` 自动包含所有插件、示例与 SDK 声明，启用 strict、未使用符号检查和声明检查。
- `packages/sdk/index.d.ts` 是 SDK 声明入口；`globals.d.ts` 加载运行时模块声明，`ply:plugins` 关联各插件的真实输入、输出及导出接口。
- 已知插件句柄提供具体参数与返回类型；动态插件和外部数据使用 unknown，经校验或明确的类型收窄后使用。
- memory 的输入使用 action 区分联合：记住/查找用 key，遗忘只用 id + expected_version；错误组合必须在类型层与运行时均被拒绝。
- lint 不接受 any、忽略类型错误或跳过声明检查；类型契约的正反例放入 `type-tests/`。
- 依赖和版本锁在根 `package-lock.json`；开发和 CI 均运行 `npm ci --ignore-scripts`、`npm run check`。check 包含插件可见面检查：插件、示例、SDK 和运行时 prelude 中不出现 fiber、`ply:lifecycle`、全局 `ctx` 或 `.loaded`。

## SPEC-FIBER：归属、错误与清理

- 每次执行是独立 fiber；异步续体、插件调用、事件监听器和原生桥都携带真实调用归属。fiber 只在内核：插件 API 不暴露 fiber、调用方或祖先。
- 同插件共享模块状态和 JS 线程，错误隔离不回滚已发生的 IO 或共享状态。
- 每个 Spawn 有唯一 Exit；终态区分成功、失败、取消和预算超限。父取消传播到子，独立兄弟继续。
- 取消或预算超限时 Exit 立即发出，随后有界清理：调用等待的非自身创建的 Promise 与宿主操作以 AbortError（code `ERR_PLY_CANCELLED`）拒绝，它自身创建的叶子 Promise 被拒绝，被强制终止的同步段留下的 async 帧也被拒绝；每个取消前已存在的 async 帧恰好收到一次 AbortError（取消后首次恢复，或同步宿主调用当场抛出），清理中新开始的帧不受影响，catch/finally 照常运行并可 await；只中止取消那一刻已存在的工作；清理期间不能发起新的子调用。整段两秒期限，结束或超时后回收定时器、HTTP、宿主任务和续体。被终止的同步段内部的 finally 不执行。
- onDispose 在 realm 回收时运行一次：`Kernel::close` 先取消在途调用并等待其清理，再按加载完成逆序调用各插件 onDispose，然后关闭 realm；onLoad 失败后同样调用并回收 realm。两秒期限。Cleanup 事件记录 owner（onDispose 为空）与终态。
- 临时 realm 无论成功、失败或取消均回收。内存预算是全进程水位，不宣称逐 fiber 堆隔离。

## SPEC-AGENT：模型、委派、报告和用量

- 模型层在 JSC 内运行内嵌 pi-ai，通过 Rust HTTP/TLS 传输；默认 DeepSeek，MiMo 显式选择。密钥只从宿主读取。
- 根 agent 默认最多两个直属子 agent；agent 插件在审批后、发起子调用前同步检查并计数（同 realm 单线程，原子），失败、完成、替换和恢复不退还配额，超额成为工具错误且不产生 Spawn。
- 父 agent 为每次委派生成一次性凭据，放入子调用输入的 `delegation`（总是覆盖模型提供的值）；子 agent 凭它继承策略上限、报告要求、用量链和父会话快照。未知凭据被拒绝。
- 子级工具与委派代数只能收窄；默认子级没有 memory 写入能力。max_delegation_depth 为零时不提供 agent 工具，入口也拒绝继续委派。
- 模型轮数默认不限；只有显式 max_turns 才限制任务调用轮次。
- 每个 agent 独立对话，子级接收显式任务背景与只读上下文快照。
- 完成检查覆盖停止原因、非空结果、结束标记、要求章节、代码围栏和协议残留；失败在原 agent 内修复，不承诺语义正确。
- 每次模型调用发出 `agent.model_call_started` 与 `agent.model_call` 事件，记录 usage 和用途；缺失 usage 标记为未知，不能记作零。
- 汇总区分自身与子树（委派的子 agent）；Jev 等旁路插件的模型用量只在它们自己的事件中，不计入 agent 的 total_usage。缓存命中率为 cache_read_tokens / input_tokens，按 token 加权，缺失用量时不能声称完整命中率。

## SPEC-CONTEXT：构建、压缩和恢复

- context 插件独占上下文构建；其他插件通过 context.set 提供内容并显式给出 `source`（自身插件名，`context` 保留），不传数字 order，不注册构建回调。不给 `conversation` 时修改模板。
- 顺序固定为 Rules → Project → Extensions → State → Reference；同类先 context，再按 source/key 稳定排序。
- Rules、Project、Extensions 构成冻结前缀；State、Reference 更新追加到完整工具批次之后，Reference provider 在每个完整工具批次后由 context 刷新（继承的会话不刷新）。相同内容不产生新版本。
- 会话 id 即使用会话的能力，context 不按调用方判定归属；子 agent 以 `inherit` 继承父会话快照，不能更新父会话。
- 模型工具和顺序在会话打开时冻结；预算含输入、输出预留和模型窗口上限。缓存 token 同样占用窗口。
- 压缩保留原任务、最近完整交互和当前状态；只提交经过版本检查且确实缩小上下文的摘要。
- 压缩失败保留旧历史，按配置重试和冷却；达到硬输入上限时明确失败，不能静默截断或继续超限请求。
- context 恢复格式为 2、agent 恢复格式为 4；拒绝旧格式，恢复权限、累计配额、报告状态和历史用量。tools_pending 不自动重放。

## SPEC-TOOLS：IO、事件和审批

- 模型 bash 工具使用 Ply sh；不加载其他 shell 的配置、函数或 alias。PATH 保留启动顺序并补齐常见 bin。
- 内部 utility 与重定向使用挂载 IO；宿主程序通过直接 argv 执行并保留真实宿主文件权限，挂载不是 OS 沙箱。
- read 支持 UTF-8 按行分页；create/edit/remove 保持挂载权限检查。
- bash.beforeExecute 覆盖整个工具调用，异步等待所有监听器后才能执行脚本；硬拒绝继续生效。
- 风险复核进入 approval 协调；缺少适配器返回 approval_required，不能把未获批准视为完成。审批请求的 id 由 approval 分配，只用于展示和日志，不含来源字段；请求方按提交内容（command/cwd 或 tool/cwd/input）校验返回结果。审批结果发出 `approval.result` 事件。
- 插件事件信封为 `{ id, name, source: { plugin }, data }`；内核记录事件时附上 fiber。
- TUI 仅通过 agent 对话并提供审批交互，agent 自行管理工具白名单。有活动会话时回答审批，否则返回 pending。读取、写入与执行均走对应审批路径。
- bypass 必须由宿主 CLI 显式选择，发出 `approval.bypassed` 事件；不扩大工具、委派、挂载权限，不跳过硬拒绝或 Jev 评估。
- Jev 可选订阅安全事件；缺失凭据、超时或不确定结果进入环境确认，不能让模型自己批准。

## SPEC-SKILLS-MEMORY：扩展与持久化记忆

- skill 从启动目录与插件 paths 参数发现 SKILL.md，并默认包含宿主配置的 ~/.agents/skills；只把元数据交给 context.Extensions。
- 完整 skill 指令由模型通过 read 分页读取，不能扩大工具、审批或委派权限。
- memory 存于 ~/.ply/memory，支持 user/project 域；Git common-dir 标识让同仓库 worktree 共用项目记忆。
- remember 使用 key + content；不同内容替换要求 expected_version。recall 使用精确 key，可省略 key 浏览，跨域同名须指定 scope。
- forget 只接受 id + expected_version，由存储层定位当前 user/project 中的 ID；未知 ID no-op，旧 ID 不得删除同 key 重建的条目。
- 显式记住和遗忘必须来自用户要求，不自动提取事实，不另发模型请求。
- 存储使用私有文件权限、加锁与原子 generation；遗忘清理旧内容，失败应如实返回 cleanup_pending。
- context 负责 Reference 注入和工具批次后的刷新，memory 不推送刷新；失败诊断不能使任务无条件中断。记忆是参考数据，不能提升指令或权限优先级。
- 写入权限只由 agent 授权决定（子 agent 默认没有 memory 工具），内核不按调用链检查。

## 验收矩阵

Ply 的当前范围是 macOS 上的 Rust/JSC agent harness：CLI、脚本和 MCP 共用同一个内核，TS 插件提供工具与上下文，DeepSeek/MiMo 通过 JSC 内嵌 pi-ai 和 Rust HTTP 调用。下面按系统行为列出验收路径，不把模型评审报告的正确率当作系统验收。

| 能力 | 已实现的完整路径 | 验收来源 |
|---|---|---|
| 使用入口 | tools 描述插件；call 返回输出/错误；run 运行脚本；MCP 提供工具服务；commands 列出注册入口；插件可注册顶层命令，tui 启动面板 | [CLI 端到端](crates/ply-cli/tests/entrypoints.rs)、[真实 PTY](crates/ply-cli/tests/tui.rs) |
| 插件与命令模块 | ply:plugins 显式导入实时目录，移除旧全局 tools；ply:commands 只在 onLoad 注册，原生 realm 归属、重复拒绝/失败撤销、独立 fiber 与取消清理 | [模块与命令](rfcs/0004-commands.md)、[内核验收](crates/ply-kernel/tests/commands.rs) |
| MCP 请求处理 | 单行 JSON-RPC 并发处理；非法 JSON、版本/方法/id、参数对象返回明确错误；合法通知和入站响应不回包；坏请求后继续服务 | [CLI 端到端](crates/ply-cli/tests/entrypoints.rs)、[入口实现](crates/ply-cli/src/main.rs) |
| 插件事件 | onLoad 声明/订阅命名空间事件；运行时异步分发，监听器独立子 fiber；异常/超时/取消阻止 bash 执行；失败加载回收注册 | [事件实现](crates/ply-kernel/src/events.rs)、[hook 集成](crates/ply-kernel/tests/kernel.rs) |
| 插件与句柄 | TS 签名转工具定义、具名/默认稳定插件句柄、onLoad 接口按插件挂载、具名导入决定 onLoad 顺序、共享闭包、重复/加载失败处理、临时 realm 回收；插件可见面不含 fiber/ctx/lifecycle/.loaded | [内核集成](crates/ply-kernel/tests/kernel.rs)、[加载顺序与回收](crates/ply-kernel/tests/loading.rs)、[签名实现](crates/ply-kernel/src/signature.rs)、[多文件模块回归](crates/ply-kernel/tests/plugin_modules.rs) |
| 文件和进程 | read 分页、create/edit/remove、只读挂载拒写、内部 Ply sh 执行与超时/取消；磁盘/内存/SQLite 后端行为一致 | [入口测试](crates/ply-cli/tests/entrypoints.rs)、[存储一致性](crates/ply-storage/tests/conformance.rs)、[IO](crates/ply-io/tests/io_layer.rs) |
| fiber 调度与归属 | 共享 Promise 按消费者归属；并发父子关系、方法调用/timer/续体保持调用身份，由 FiberHook 观察而非 JS；每个 Spawn 有唯一 Exit | [JSC 集成](crates/ply-jsc/tests/node_outlet.rs)、[内核集成](crates/ply-kernel/tests/kernel.rs) |
| 错误与取消 | 普通错误、rejection、定时器错误、宿主 panic、CPU/内存超限有明确终态；父取消传播到子，独立兄弟继续；同步 CPU/exec 等待响应取消 | [JSC 集成](crates/ply-jsc/tests/node_outlet.rs)、[内核取消](crates/ply-kernel/tests/kernel.rs) |
| 清理生命周期 | 取消/预算超限后 catch/finally 在两秒有界清理中运行（AbortError）、父子都清理、清理中拒绝新调用；超时中断 CPU/同步 exec/异步等待；清理完成回收未 await 的定时器/宿主任务；onDispose 导出 hook 在 realm 回收时运行一次；独立 Cleanup 记录 owner 与终态 | [JSC 清理](crates/ply-jsc/tests/node_outlet.rs)、[内核清理](crates/ply-kernel/tests/cleanup.rs) |
| 环境审批协调 | approval.request 保持 bash 协议，toolRequest 覆盖模型工具调用；读取前审批，决定与原参数绑定；TUI 只调用 agent 并展示审批；默认主/子 agent 分别七/五个工具；无环境返回 approval_required | [审批协议](rfcs/0003-approval.md)、[模型工具门禁](crates/ply-kernel/tests/approval.rs)、[真实 PTY](crates/ply-cli/tests/tui.rs) |
| 持久化记忆 | memory 显式 remember/recall/forget，用户/项目分域，私有文件与原子 generation；context.Reference 快照和所属会话刷新；key 记住/查找、id+version 遗忘 | [Memory](rfcs/0008-memory.md)、[集成测试](crates/ply-kernel/tests/memory.rs)、[存储实现](crates/ply-jsc/src/memory.rs) |
| skills | 插件 onLoad 参数配置来源，启动目录发现；元数据写入 context.Extensions 模板，由 context 继承和恢复；通过既有 read 读取完整 SKILL.md，保留审批和挂载边界 | [Skills](rfcs/0006-skills.md)、[集成测试](crates/ply-kernel/tests/skills.rs) |
| Jev 安全检查 | 可选订阅 bash.beforeExecute，原生 state/questions/Noul，阈值建议、结构化结果、异常或不确定转审批；私有密钥及插件模型用量归属 | [Jev 说明](rfcs/0010-jev.md)、[原生 API 集成](crates/ply-kernel/tests/jev.rs) |
| agent 委派 | 独立对话、委派凭据、工具白名单、子级只能收窄、代数限制；累计直属子数由 agent 插件同步准入，失败/完成/替换/恢复不退款；模型轮数默认不限 | [agent 集成](crates/ply-kernel/tests/agent.rs)、[配额并发](crates/ply-kernel/tests/kernel.rs) |
| 报告完成 | 正常停止、非空、结束标记、围栏、协议残留、要求章节；失败由原 agent 修复，截断工具参数不执行 | [agent 集成及模型 fixture](crates/ply-kernel/tests/agent.rs) |
| 上下文 | Rules/Project/Extensions/State/Reference 中央排序，固定前缀、会话隔离、增量状态；工具快照、预算、完整交互压缩及失败恢复 | [context 集成](crates/ply-kernel/tests/context.rs)、[agent 集成](crates/ply-kernel/tests/agent.rs) |
| 失败恢复 | 私有原子快照、排他恢复、agent 格式 4 / context 格式 2，拒绝旧版本；恢复对话、权限、累计配额、报告状态及历史用量；tools_pending 禁止自动重放 | [agent 恢复用例](crates/ply-kernel/tests/agent.rs)、[宿主快照](crates/ply-jsc/src/checkpoints.rs) |
| 模型运行时 | 内嵌 DeepSeek/MiMo 接入、SSE 流读取、reasoning/tool/usage 透传、并发隔离、取消、体积上限；密钥只从宿主取，模型调用无需外部 Node | [SDK/本地 HTTP 集成](crates/ply-jsc/tests/model_runtime.rs)、[HTTP 实现](crates/ply-jsc/src/http.rs)、[模型选择](crates/ply-cli/src/main.rs) |
| 用量与事件 | agent.model_call_started/agent.model_call 事件配对、自身/子树汇总、缺失用量标记、按 token 加权缓存命中率；JSONL 写 Spawn/Exit/Cleanup/Plugin | [agent 集成](crates/ply-kernel/tests/agent.rs)、[事件实现](crates/ply-core/src/fiber.rs) |

## 验收命令

```bash
npm ci --ignore-scripts --registry=https://registry.npmjs.org
npm run check
cargo test --workspace --locked --offline
cargo build -p ply-cli --locked --offline
```

测试中的模型 fixture 和本地 HTTP 服务不产生供应商调用，它们验证系统行为与协议处理。真实 DeepSeek/MiMo 的供应商连通性、用量和进程审计应另留运行轨迹；本地测试通过不能代替供应商实测，也不代表所有 provider 已接入。主 agent 开放 API key 认证的 DeepSeek 和 MiMo；Jev 作为独立的原生决策 API 订阅插件接入。

MCP 的请求字段与错误码依据 [JSON-RPC 2.0](https://www.jsonrpc.org/specification)；这里只列出 Ply 已实现的 stdio 工具服务，不声称实现全部 MCP 扩展。

## 行为边界

- 插件代码是可信执行边界；模型 bash tool 的文件访问通过内部 sh 和挂载 IO；其他插件直接使用的宿主子进程不是 OS 沙箱。
- 同插件共享闭包与一个 JS 线程；错误隔离不回滚已发生的共享状态或 IO。内存阈值是进程水位。
- 每次调用的清理用 finally，取消后也会运行；onDispose 只用于 realm 回收。必须完成的清理需要 await；清理结束后，未等待的后台工作会取消，不能依靠它继续运行。
- onLoad 失败时，它此前调用其他插件方法造成的状态不回滚；宿主应在 `ready()` 成功后才开始服务（CLI 如此）。
- 恢复保留安全交互点；首次强制 VM 中断不保证生成新快照，已有 tools_pending 快照不自动重放。旧快照格式明确拒绝。
- 上下文 token 是保护性估算；缓存读取仍占上下文。报告完整性校验只验证格式和完成状态，不验证模型结论真伪。
