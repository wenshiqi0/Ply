# Ply SPEC

状态：当前规范。本文定义系统必须维持的契约；RFC 记录决策、动机与变更过程。

## 基于规范开发

1. 开发前确认本规范中的行为、类型、权限与验收来源。
2. 改变行为或公共接口先提出 RFC，明确兼容范围和验证方式；确认后实现。
3. 实现、SDK 类型、测试与 SPEC 同步修改。已实现 RFC 的有效规则同步回本文；不能只留下 RFC。
4. 内部重构保留已确认的 CLI、插件参数、`ply:plugins` 接口与安全边界。
5. 自动检查通过后记录验证结果。fixture 测试不能代替真实供应商或交互验收。

RFC 索引见 [docs/rfcs/README.md](docs/rfcs/README.md)，使用与开发指南见 [docs/](docs/README.md)。插件模块化和类型治理见 [RFC 0011](docs/rfcs/0011-plugin-modules-and-types.md)；插件只见能力、fiber 只在内核见 [RFC 0012](docs/rfcs/0012-capabilities-not-scheduling.md)。

## SPEC-PLUGINS：插件与模块

- 一个目录是一个插件，入口按 `main.ts`、`index.ts`、`main.js`、`index.js` 选择。目录名是插件名。
- 入口具名导出 `onHandle(input)`、`onLoad(options)`、`onDispose()` 中的任意几个；`onHandle` 的 JSDoc、参数、返回类型生成描述与工具 schema。
- `onLoad` 只运行一次，返回的数据与方法通过插件句柄提供；方法是所属 realm 内的异步 RPC。`onDispose` 在 realm 回收时运行一次。
- 插件只见能力，不见调度：运行时不提供 `fiber`、`ctx` 全局和 `ply:lifecycle`，句柄没有 `.loaded`；插件之间通过输入、返回值和持有的值（如会话 id）建立关系，不按调用方判定归属。
- 入口可用插件目录内的相对 import 拆分模块。同一插件的模块共享一个 realm 和模块缓存，不能因拆文件改变 fiber 归属、状态或加载时序。ES 模块的导入是 live binding：每次使用都读取导出模块当时的值，循环导入按 Node 语义工作。
- 内置插件在二进制中嵌入完整模块目录；它们的 npm 依赖处于开发模式（[RFC 0013](docs/rfcs/0013-resources-and-npm.md) 第一步）：运行时以只读方式读取仓库根目录的 `node_modules`（路径在编译期确定，挂载在代码命名空间的 `/ply/node_modules`），cargo 构建前必须先 `npm ci`，打包模式实现前二进制不能脱离仓库运行。构建和运行都不启动外部 Node；目录插件、脚本与内置插件使用同一模块解析规则。
- 模块解析按 Node：裸模块名从导入方目录逐级向上查找 `node_modules`，读取 `exports`（条件为 `node`、`import`/`require`、`module-sync`、`default`，按包中列出的顺序匹配；不匹配 `node-addons`，等同 `node --no-addons`），否则用 `main` 与 `index`；`.js` 按最近 package.json 的 `type` 区分 ESM/CJS，没有 `type` 时按 Node 22 的语法检测；支持 CJS（`require`、`module`、`exports`、`__filename`、`__dirname`）、ESM 导入 CJS（`default` 为 `module.exports`）、require 不含顶层 await 的 ESM（有默认导出时返回的对象带 `__esModule: true`）、JSON（ES 模块导入时必须带 `with { type: "json" }`）。插件内的相对导入不越出插件目录，包内的相对导入不越出该包；目录插件和脚本祖先目录中的 `node_modules` 以只读方式挂载；未实现的内置模块报 `ERR_UNKNOWN_BUILTIN_MODULE`。转译结果按路径与内容哈希在进程内缓存。
- 运行时边界：Ply 只提供资源能力（进程环境、进程执行、文件、网络、终端）与内核能力（`ply:plugins`、`ply:events`、`ply:commands`）。外部代码依赖经 node 模块解析从 node_modules 引入，开发模式直接读取 node_modules，打包时随应用嵌入；存储由插件在 fs 上实现，跨进程互斥使用 fs 的文件锁扩展（OS 排他咨询锁，跨进程有效，关闭句柄或进程退出时释放）；密钥通过环境变量注入。`ply:models` 已删除；`ply:memory-store`、`ply:checkpoints`、`ply:http`、`ply:skills`、`ply:markdown` 按 [RFC 0013](docs/rfcs/0013-resources-and-npm.md) 的后续步骤迁出，迁移完成前按各自现行契约运行。
- 签名读取支持插件目录内相对文件中的具名类型导入与别名；递归类型保守收敛为 unknown。未支持的 TypeScript 高级类型不得声称具备运行时参数校验。
- `main.ts` 保留入口与装配，`types.ts` 声明输入、输出、onLoad 参数和公共导出；较复杂的策略、存储、渲染、统计等放入职责模块。
- 每个 TS/声明文件最多 250 个非空、非注释行；不得通过合并多条语句或全局关闭检查规避约束。
- 具名句柄由 `import { context, agent, memory } from "ply:plugins"` 引入；动态目录由默认 import 引入。插件的 onLoad 在它具名导入的插件完成 onLoad 后才开始；被导入的插件加载失败时依赖方加载失败，相互导入成环报错；默认导入不建立顺序。对句柄的调用和方法调用等待目标加载完成；`events.on` 等待事件被声明或所属插件 onLoad 结束。
- `ply:commands` 在 onLoad 注册宿主入口，不扩大模型工具权限。默认工具由 context 收集，agent 按授权收窄。
- Node 风格 API 由运行时提供；编辑器的 Node 类型不意味着支持完整 Node 运行时。支持范围以运行时及 `tests/node` 用例为准。当前提供的内置模块：`fs`、`fs/promises`、`path`、`path/posix`、`process`、`buffer`、`events`、`util`、`util/types`、`os`、`timers`、`timers/promises`、`child_process`、`stream`、`stream/promises`、`string_decoder`；全局另有 `global`、`atob`、`btoa`。`net`、`http`、`http2` 与进程、系统 API 的补齐按 [RFC 0015](docs/rfcs/0015-node-io-and-net.md) 分阶段进行。
- 运行时自己的 Node 模块是 `crates/ply-jsc/src/node/` 下的 TypeScript ES 模块，由内置的 OXC 每个进程转译一次，首次导入时求值，导出以 `@types/node` 的类型约束。它们以 `ply:runtime` 导入 prelude 的内部函数，插件代码解析不到这个名字。EventEmitter 与各流类是普通构造函数，`X.call(this)` 配合 `util.inherits` 的旧式子类可用。`process.platform`、`process.arch` 与 `os` 报告宿主的真实平台。
- Web 全局 `fetch`、`Headers`、`Request`、`Response`、`URL`、`URLSearchParams`、`AbortController`、`AbortSignal`、`ReadableStream`、`FormData`、`EventTarget`、`Event`、`DOMException`、`structuredClone` 由运行时原生实现：URL 用 Rust `url` crate 按 WHATWG 标准解析，fetch 走 Rust HTTP/TLS，只支持 http/https，重定向到其他源时去掉凭据头，请求体和累计响应体各限 16 MiB，网络失败为 `TypeError: fetch failed`；`AbortSignal` 中止得到它的 reason，所属调用被取消时得到 AbortError（`ERR_PLY_CANCELLED`）。FormData 只保存字符串，不支持 multipart 请求体；不支持 byte stream。
- Ply 运行时以 Node 为基线，可以作为 Node 的超集：已提供的 Node API 遵循 Node 语义；Node 缺少的资源能力（如文件锁）作为扩展加在对应的 Node 对象上，不另起 `ply:` 模块。
- 对齐的 Node 版本只在根目录 `.node-version` 定义一处（当前 24.14.0），`process.version`、CI 的 Node 和兼容用例都取自它（[RFC 0014](docs/rfcs/0014-node-compat-suite.md)）。已提供的 Node API 由 `tests/node/<类别>/` 的用例保障：期望值由该版本的 Node 录制、CI 复核，Ply 用 `ply run` 跑同一批用例比对。只有 Ply 的进程模型与沙箱（pid、argv[0]、精选环境、`HOME` 与临时目录、挂载、无原生扩展、插件根与包边界）可以造成差异，这类用例另有 `.ply.expected`，并在用例开头用 `// ply-divergence:` 写明理由。

## SPEC-TYPES：类型与静态检查

- 根 `tsconfig.json` 自动包含所有插件、示例与 SDK 声明，启用 strict 与未使用符号检查；`skipLibCheck` 跳过 d.ts 检查，因为 `node_modules` 中第三方声明在 Ply 的配置下无法通过。`packages/sdk` 的声明由 `scripts/check-sdk-types.mjs` 在同一程序中关闭 `skipLibCheck` 单独严格检查。
- `packages/sdk/index.d.ts` 是 SDK 声明入口；`globals.d.ts` 加载运行时模块声明，`ply:plugins` 关联各插件的真实输入、输出及导出接口。
- 已知插件句柄提供具体参数与返回类型；动态插件和外部数据使用 unknown，经校验或明确的类型收窄后使用。
- memory 的输入使用 action 区分联合：记住/查找用 key，遗忘只用 id + expected_version；错误组合必须在类型层与运行时均被拒绝。
- lint 不接受 any、忽略类型错误或跳过 Ply 自己的声明检查；类型契约的正反例放入 `type-tests/`。
- `@types/node` 的主版本与 `.node-version` 相同、次版本不高于它，由 `npm run check` 核对。
- 依赖和版本锁在根 `package-lock.json`；开发和 CI 均运行 `npm ci --ignore-scripts`、`npm run check`。check 包含插件可见面检查：插件、示例、SDK 和运行时 prelude 中不出现 fiber、`ply:lifecycle`、全局 `ctx`、`.loaded` 或已删除的 `ply:models`。运行时的 npm 依赖写在根 `dependencies`（pi-ai 锁定 0.82.1）。

## SPEC-FIBER：归属、错误与清理

- 每次执行是独立 fiber；异步续体、插件调用、事件监听器和原生桥都携带真实调用归属。fiber 只在内核：插件 API 不暴露 fiber、调用方或祖先。
- 同插件共享模块状态和 JS 线程，错误隔离不回滚已发生的 IO 或共享状态。
- 每个 Spawn 有唯一 Exit；终态区分成功、失败、取消和预算超限。父取消传播到子，独立兄弟继续。
- 取消或预算超限时 Exit 立即发出，随后有界清理：调用等待的非自身创建的 Promise 与宿主操作以 AbortError（code `ERR_PLY_CANCELLED`）拒绝，它自身创建的叶子 Promise 被拒绝，被强制终止的同步段留下的 async 帧也被拒绝；每个取消前已存在的 async 帧恰好收到一次 AbortError（取消后首次恢复，或同步宿主调用当场抛出），清理中新开始的帧不受影响，catch/finally 照常运行并可 await；只中止取消那一刻已存在的工作；清理期间不能发起新的子调用。整段两秒期限，结束或超时后回收定时器、HTTP、宿主任务和续体。被终止的同步段内部的 finally 不执行。
- onDispose 在 realm 回收时运行一次：`Kernel::close` 先取消在途调用并等待其清理，再按加载完成逆序调用各插件 onDispose，然后关闭 realm；onLoad 失败后同样调用并回收 realm。两秒期限。Cleanup 事件记录 owner（onDispose 为空）与终态。
- 临时 realm 无论成功、失败或取消均回收。内存预算是全进程水位，不宣称逐 fiber 堆隔离。

## SPEC-AGENT：模型、委派、报告和用量

- agent 从 npm 导入 pi-ai，首次模型请求时才加载，经运行时的 fetch 和 Rust HTTP/TLS 传输；默认 DeepSeek，MiMo 显式选择。密钥取自 profile 的 `apiKeyEnv` 所指的 `process.env` 变量，不使用 pi-ai 的其他环境变量或凭据文件；CLI 只把宿主已设置的 `DEEPSEEK_API_KEY`、`MIMO_API_KEY` 传入插件环境。
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

- 模型 bash 工具使用 Ply sh；不加载其他 shell 的配置、函数或 alias。PATH 保留启动顺序并补齐常见 bin。模型命令的环境是插件 `process.env` 去掉所有以 `_API_KEY` 结尾的变量；`ply:sh` 的 `exec` 可传显式 `env`，缺省为 `process.env`。
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

Ply 的当前范围是 macOS 上的 Rust/JSC agent harness：CLI、脚本和 MCP 共用同一个内核，TS 插件提供工具与上下文，DeepSeek/MiMo 通过 npm 上的 pi-ai、运行时的 fetch 和 Rust HTTP 调用。下面按系统行为列出验收路径，不把模型评审报告的正确率当作系统验收。

| 能力 | 已实现的完整路径 | 验收来源 |
|---|---|---|
| 使用入口 | tools 描述插件；call 返回输出/错误；run 运行脚本；MCP 提供工具服务；commands 列出注册入口；插件可注册顶层命令，tui 启动面板 | [CLI 端到端](crates/ply-cli/tests/entrypoints.rs)、[真实 PTY](crates/ply-cli/tests/tui.rs) |
| 插件与命令模块 | ply:plugins 显式导入实时目录，移除旧全局 tools；ply:commands 只在 onLoad 注册，原生 realm 归属、重复拒绝/失败撤销、独立 fiber 与取消清理 | [模块与命令](docs/rfcs/0004-commands.md)、[内核验收](crates/ply-kernel/tests/commands.rs) |
| MCP 请求处理 | 单行 JSON-RPC 并发处理；非法 JSON、版本/方法/id、参数对象返回明确错误；合法通知和入站响应不回包；坏请求后继续服务 | [CLI 端到端](crates/ply-cli/tests/entrypoints.rs)、[入口实现](crates/ply-cli/src/main.rs) |
| 插件事件 | onLoad 声明/订阅命名空间事件；运行时异步分发，监听器独立子 fiber；异常/超时/取消阻止 bash 执行；失败加载回收注册 | [事件实现](crates/ply-kernel/src/events.rs)、[hook 集成](crates/ply-kernel/tests/kernel.rs) |
| 插件与句柄 | TS 签名转工具定义、具名/默认稳定插件句柄、onLoad 接口按插件挂载、具名导入决定 onLoad 顺序、共享闭包、重复/加载失败处理、临时 realm 回收；插件可见面不含 fiber/ctx/lifecycle/.loaded | [内核集成](crates/ply-kernel/tests/kernel.rs)、[加载顺序与回收](crates/ply-kernel/tests/loading.rs)、[签名实现](crates/ply-kernel/src/signature.rs)、[多文件模块回归](crates/ply-kernel/tests/plugin_modules.rs) |
| 文件和进程 | read 分页、create/edit/remove、只读挂载拒写、内部 Ply sh 执行与超时/取消；磁盘/内存/SQLite 后端行为一致 | [入口测试](crates/ply-cli/tests/entrypoints.rs)、[存储一致性](crates/ply-storage/tests/conformance.rs)、[IO](crates/ply-io/tests/io_layer.rs) |
| fiber 调度与归属 | 共享 Promise 按消费者归属；并发父子关系、方法调用/timer/续体保持调用身份，由 FiberHook 观察而非 JS；每个 Spawn 有唯一 Exit | [JSC 集成](crates/ply-jsc/tests/node_outlet.rs)、[内核集成](crates/ply-kernel/tests/kernel.rs) |
| 错误与取消 | 普通错误、rejection、定时器错误、宿主 panic、CPU/内存超限有明确终态；父取消传播到子，独立兄弟继续；同步 CPU/exec 等待响应取消 | [JSC 集成](crates/ply-jsc/tests/node_outlet.rs)、[内核取消](crates/ply-kernel/tests/kernel.rs) |
| 清理生命周期 | 取消/预算超限后 catch/finally 在两秒有界清理中运行（AbortError）、父子都清理、清理中拒绝新调用；超时中断 CPU/同步 exec/异步等待；清理完成回收未 await 的定时器/宿主任务；onDispose 导出 hook 在 realm 回收时运行一次；独立 Cleanup 记录 owner 与终态 | [JSC 清理](crates/ply-jsc/tests/node_outlet.rs)、[内核清理](crates/ply-kernel/tests/cleanup.rs) |
| 环境审批协调 | approval.request 保持 bash 协议，toolRequest 覆盖模型工具调用；读取前审批，决定与原参数绑定；TUI 只调用 agent 并展示审批；默认主/子 agent 分别七/五个工具；无环境返回 approval_required | [审批协议](docs/rfcs/0003-approval.md)、[模型工具门禁](crates/ply-kernel/tests/approval.rs)、[真实 PTY](crates/ply-cli/tests/tui.rs) |
| 持久化记忆 | memory 显式 remember/recall/forget，用户/项目分域，私有文件与原子 generation；context.Reference 快照和所属会话刷新；key 记住/查找、id+version 遗忘 | [Memory](docs/rfcs/0008-memory.md)、[集成测试](crates/ply-kernel/tests/memory.rs)、[存储实现](crates/ply-jsc/src/memory.rs) |
| skills | 插件 onLoad 参数配置来源，启动目录发现；元数据写入 context.Extensions 模板，由 context 继承和恢复；通过既有 read 读取完整 SKILL.md，保留审批和挂载边界 | [Skills](docs/rfcs/0006-skills.md)、[集成测试](crates/ply-kernel/tests/skills.rs) |
| Jev 安全检查 | 可选订阅 bash.beforeExecute，原生 state/questions/Noul，阈值建议、结构化结果、异常或不确定转审批；私有密钥及插件模型用量归属 | [Jev 说明](docs/rfcs/0010-jev.md)、[原生 API 集成](crates/ply-kernel/tests/jev.rs) |
| agent 委派 | 独立对话、委派凭据、工具白名单、子级只能收窄、代数限制；累计直属子数由 agent 插件同步准入，失败/完成/替换/恢复不退款；模型轮数默认不限 | [agent 集成](crates/ply-kernel/tests/agent.rs)、[配额并发](crates/ply-kernel/tests/kernel.rs) |
| 报告完成 | 正常停止、非空、结束标记、围栏、协议残留、要求章节；失败由原 agent 修复，截断工具参数不执行 | [agent 集成及模型 fixture](crates/ply-kernel/tests/agent.rs) |
| 上下文 | Rules/Project/Extensions/State/Reference 中央排序，固定前缀、会话隔离、增量状态；工具快照、预算、完整交互压缩及失败恢复 | [context 集成](crates/ply-kernel/tests/context.rs)、[agent 集成](crates/ply-kernel/tests/agent.rs) |
| 失败恢复 | 私有原子快照、排他恢复、agent 格式 4 / context 格式 2，拒绝旧版本；恢复对话、权限、累计配额、报告状态及历史用量；tools_pending 禁止自动重放 | [agent 恢复用例](crates/ply-kernel/tests/agent.rs)、[宿主快照](crates/ply-jsc/src/checkpoints.rs) |
| 模型运行时 | agent 的 pi.ts 经 npm pi-ai 与运行时 fetch 接入 DeepSeek/MiMo、SSE 流读取、reasoning/tool/usage 透传、并发隔离；密钥来自插件环境，模型 shell 看不到；模型调用无需外部 Node | [本地 HTTP 集成](crates/ply-jsc/tests/model_runtime.rs)、[密钥与 bash](crates/ply-kernel/tests/kernel.rs)、[模型选择](crates/ply-cli/src/main.rs) |
| Node 兼容用例 | 按类别（modules、path、fs、buffer、events、util、process、os、timers、child_process、web）保障已提供的 Node API；期望值由 `.node-version` 的 Node 录制、CI 复核；有意差异登记理由 | [用例](tests/node/)、[Ply 比对](crates/ply-cli/tests/node_compat.rs)、[录制与复核](scripts/node-compat.mjs) |
| 模块与 Web 全局 | node_modules 逐级查找、exports 条件与模式、CJS/ESM 互操作、JSON、语法检测、包边界、live binding；真实 pi-ai 依赖图加载；URL/Headers/流、fetch 的 JSON、重定向去凭据、流读取与 clone、AbortSignal、调用取消、体积上限 | [解析](crates/ply-jsc/tests/node_modules.rs)、[Web 全局](crates/ply-jsc/tests/web.rs)、[解析实现](crates/ply-jsc/src/resolve.rs)、[Web 实现](crates/ply-jsc/src/web.js) |
| 用量与事件 | agent.model_call_started/agent.model_call 事件配对、自身/子树汇总、缺失用量标记、按 token 加权缓存命中率；JSONL 写 Spawn/Exit/Cleanup/Plugin | [agent 集成](crates/ply-kernel/tests/agent.rs)、[事件实现](crates/ply-core/src/fiber.rs) |

## 验收命令

```bash
npm ci --ignore-scripts --registry=https://registry.npmjs.org
npm run check
npm run compat:check
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
