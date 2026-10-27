# RFC 0001: ply 设计

状态：已实现。当前有效契约以 [SPEC.md](../../SPEC.md) 为准；本文件保留方案细节、边界与操作示例。

> 已被 [RFC 0012](0012-capabilities-not-scheduling.md) 取代的部分：`fiber`、`ctx` 全局，`ply:lifecycle`（`current()`、`onDispose(cb)`），`plugins.x.loaded`，`fiber.record` 记录，以及内核执行的直属子 agent 配额。下文涉及这些的段落只保留为历史记录。

## 问题与决策

把模型工具、调度和 IO 分层：Rust 负责可验证的 fiber 归属及原生资源，JSC 承载插件状态与模型循环。

本文记录已经和作者对齐的设计决策。代码里每个 crate 的 `lib.rs` 顶部也有对应的 "Important decisions" 注释。

## 1. 插件：两个钩子

- 一个插件是一个目录，`main.ts`（或 `index.ts` / `main.js` / `index.js`）用具名导出提供最多两个钩子。插件之间相互独立，不聚合、不声明依赖。
  - **`onHandle(input)`**：每次调用运行一次（一个 fiber）。
  - **`onLoad(options)`**：加载时运行一次，宿主传入该插件的 JSON 参数，未配置时为 `{}`；返回的东西挂到全局 `ctx` 上。
- 插件声明得越少越好：
  - **名字**是目录名；
  - **描述**是 `onHandle` 上的 JSDoc（没有 `onHandle` 时取 `onLoad` 上的）；
  - **输入和输出**的描述就是 `onHandle` 的参数类型和返回类型（`Promise<T>` 取 `T`）。ply 加载插件时用 oxc 从签名里读出来，同一文件里的 `interface` / `type` 会展开，读不懂的部分记为 `unknown`。
- 没有 `onHandle` 的插件不能作为工具调用（不出现在 MCP 的 tools/list 里），仍可在 onLoad 中通过 `ply:commands` 注册 CLI 命令。
- 参数校验由插件自己做。模型工具授权与审批由 agent/approval 管理；fiber 归属、回调注册归属和子任务配额由内核执行。

## 2. fiber、`ply:plugins` 和 `ctx`

- **`plugins.<name>`**：每个已加载的插件。
  - `plugins.bash(input)` 是一个惰性的 thenable：持有它什么都不会发生，`await`（`then`）的那一刻就是调用 `onHandle`，内核把它作为当前 fiber 的子 fiber 来跑。同一个句柄 await 两次只执行一次，和 Promise 一样。并发直接用 `Promise.all`。
  - `plugins.<name>.loaded` 是单独挂着的一个 promise，在该插件的 `onLoad` 返回时 resolve。
  - `description`，以及有 `onHandle` 时的 `parameters`（作为工具时参数的 JSON Schema）和 `wrapsInput`。schema 由 core 的 `PluginSpec::tool_schema` 统一生成，MCP 也用它。工具参数总是对象，输入不是对象的插件，参数包成 `{ "input": ... }`。
  - `Object.keys(plugins)` 列出所有插件。
- **具名句柄**：`import { context, read } from "ply:plugins"` 与默认目录对应成员相同。onLoad 的数据和方法按来源插件解析，例如 `context.Type`、`context.set`；动态方法调用携带原生来源、fiber 和宿主客户端身份，不受其他插件同名导出影响。
- **`ctx`**：所有插件的 `onLoad` 返回的成员，平铺在一起，不按插件名分隔。
  - 数据成员是 `onLoad` 返回时的一份拷贝；函数成员在挂载它的插件的 realm 里执行，所以调用总是返回 promise（跨线程只能如此）。
  - 同名成员，后挂载的覆盖先挂载的（按 `onLoad` 完成的先后），这是有意保留的覆盖方式。
- **加载**：
  - 内核加载插件时读签名、建 realm、执行模块，然后在后台运行 `onLoad`。所以 `onLoad` 里可以 `await plugins.xxx.loaded` 等别的插件，不受加载顺序影响。
  - 加载完成时，先向各 realm 排入包含新挂载的目录消息，再唤醒 `loaded` 等待者；目录快照生成和发送期间持有插件表锁，避免旧快照覆盖新挂载。`await plugins.xxx.loaded` 返回后可立即访问它挂载到 `ctx` 的成员。
  - 宿主用 `Kernel::ready` 等所有 `onLoad` 结束。在那之后，等待一个不存在的插件会立刻失败，而不是一直挂着。
  - 调用一个还在加载的插件，会先等它加载完。
- **主动唤起只来自宿主**（`Kernel::wake`），插件之间不互相唤起。
- 嵌套深度有上限（默认 16），防止插件无限等待自己。
- **记录的是 fiber**：每个 fiber 的 spawn（id、父 fiber、插件、输入）、record（运行中主动记录的数据）和 exit（输出或错误、耗时）是一个 `FiberEvent`，交给注册的 `FiberHook`。CLI 的 `--record FILE` 把它们逐条写成 JSON lines 并 flush。`ctx` 上的函数调用不是 fiber，不自动记录。IO 层不做任何跟踪。
- **运行中的 fiber**：`await fiber.info()` 返回当前 id、parent 和从近到远的 ancestors；`await fiber.record(data)` 发出这个 fiber 的结构化记录。身份由内核从调用方取得，插件不传 id；两个方法只能在运行中的 fiber 内使用。内核不解释 data 的业务含义。

## 3. 默认插件

agent、bash、context、create、deepseek、edit、memory、mimo、read、remove、skill，和第三方插件写法完全一样，打包进二进制。`PluginSource::defaults()` 保持默认加载 DeepSeek；MiMo 通过 `PluginSource::builtin("mimo")` 显式选择。CLI 的 `--model mimo-*` 替换默认的 deepseek 接入，不让两个接入插件竞争同一个 currentModel。

| 插件 | 钩子 | 职责 |
|---|---|---|
| context | onLoad | 管理模型看到的东西：注册进来的工具、要用的模型、进行中的对话。都存在模块闭包里，不持久化 |
| deepseek | onLoad | 本机的 DeepSeek 接入：只提供上下文集成，告诉 context 用哪个模型、key 在哪个环境变量 |
| mimo | onLoad | 可选的 MiMo V2.6 接入：提供当前模型定义、Token Plan/按量服务选择和宿主密钥环境变量名 |
| bash、create、edit、read、remove | onHandle、onLoad | 各自完成一件事，并在 `onLoad` 里把自己注册到 context（`context.registerTool`） |
| skill | onLoad | 接受参数并发现技能元数据，写入 context.Extensions 模板；正文由 read 按需读取，见 [Skills](0006-skills.md) |
| agent | onHandle、onLoad | 用语言模型完成一个任务，并注册自己，允许模型通过同一工具开启子 agent |

**read** 通过 `node:fs/promises` 读取挂载存储里的 UTF-8 文本，不启动 shell。支持 `path`、从 1 开始的 `offset` 和 `limit`（默认 200 行）；返回原始文本和分页元数据，保留换行符。只限制返回的行范围，底层仍一次读取整个文件。agent 提示模型优先用它读取文件。

**context** 的接口统一通过 `import { context } from "ply:plugins"` 访问。内容块类型、固定前缀和更新机制见 [Context](0007-context.md)。保留的基础函数包括：`registerTool(name)`、`useModel(profile)`、`currentModel()`、`contextConfig()`、`openConversation(system, modelLimits?, allowed_tools?)`、`appendMessages(id, messages, observation?)`、`prepareContext(id, recovery?)`、`commitCompaction(id, revision, summary, summaryTokens?)`、`cancelCompaction(id, revision)`、`exportConversation(id)`、`restoreConversation(snapshot, modelLimits, allowed_tools, system)`、`availableTools()`、`closeConversation(id)`。`appendMessages` 仍返回系统提示、消息和已注册工具；过长工具文本仍保留头尾（上限 20000 字符）。预算、触发阈值、完整交互切分、版本和摘要提交策略都属于 context；agent 只执行摘要模型请求并记录用量，Rust 内核不解释压缩。

**上下文配置**：CLI 的 `--context-config FILE` 接受 JSON，`--max-context-tokens`、`--context-reserve-tokens`、`--context-compaction`、`--context-compact-trigger`、`--context-compact-target` 覆盖对应字段，序列化后通过内核 env 的 `PLY_CONTEXT_CONFIG` 传入 context。默认总窗口 131072、输出预留 16384，输入预算为两者之差；实际窗口不超过模型 metadata 的 contextWindow，输出不超过 maxTokens。默认输入预算达到 80% 自动压缩，目标 50%，保留最近 4 个完整交互，摘要输出上限 2048。配置校验拒绝未知字段、无效值和不一致比例。关闭自动压缩仍检查最大输入预算，不改变默认无限轮数语义。

**估算与压缩事务**：模型 metadata 从 JSC 内嵌 pi-ai 目录只读查询，不计一次付费模型调用。新会话按 UTF-8 字节数/2 加结构开销估算；有效模型 usage 的输入+输出用于校准对应历史前缀，尾部新增消息继续估算，工具集变化或摘要替换后旧锚点失效。这是预算估算，不是精确 tokenizer。每次任务模型调用前 `prepareContext` 检查预算；只切分已配齐结果的 assistant/toolCall 批次，保留原始任务与最近交互。过长前缀以完整交互为单位分批摘要，摘要请求本身也受输入预算限制。摘要由当前模型生成、不提供工具、不传额外 reasoningEffort；成功且未截断、有有效文本后按 revision 提交，合并旧摘要并释放被替换历史。空摘要、失败、无法减小历史、版本过期不提交；摘要默认额外重试 1 次、间隔 500 ms，失败后在硬预算允许时冷却 2 轮继续原任务。不能降级时由 agent 持久化完整交互的恢复记录，恢复时重新应用预算和权限；不可拆的过大原始任务或交互会报错。保留区可能阻止达到目标比例，但不允许发送超过输入预算的任务请求。各子 agent 的会话、版本和摘要独立。

**模型接入**：`ModelProfile` 可以携带显式 pi-ai `definition`、`apiKeyHeader` 和 `reasoningEffort`。内置 `ply:models` 模块优先使用显式定义，否则查内嵌 pi-ai 目录；定义里的 id/provider 必须与 profile 一致，当前 API 限定为 openai-completions。MiMo 用显式 V2.6 定义，复用 pi-ai 已有的 Xiaomi provider 和 Chat Completions 适配；`MIMO_API_KEY` 通过 Rust 宿主私有接口读取，仅在 JSC 模型调用中构造 api-key 认证头，不把密钥写进 process.env、context 或用量记录。默认思考开启，回传 reasoning_content，usage 走同一套记录。CLI 仅把非敏感的 `PLY_MODEL` / `PLY_MIMO_PROVIDER` 配置传给插件。按量和 Token Plan 的 endpoint 与密钥必须匹配；Token Plan 不从 SDK cost 占位元数据推导金额。

**JSC 模型运行时**：`packages/pi-ai/package-lock.json` 固定 pi-ai 0.82.1。构建脚本仅打包 DeepSeek 与四个 Xiaomi/MiMo provider，生成的 JS 和第三方声明随源码保存，通过 `include_str!` 嵌入二进制；Cargo 构建与模型调用不启动 Node/npm，不读取外部全局 Pi 或 `PLY_PI_AI`。第一次导入 `ply:models` 时在当前 realm 惰性实例化，后续 fiber 共用模型集合，每次请求的 context 和 AbortController 独立。bundle 使用私有浏览器作用域和构建期环境分支消除，普通插件的 process/globalThis 不变。

HTTP 由 Rust reqwest 执行，TLS 使用系统根证书，沿用宿主 HTTP(S)/ALL_PROXY/NO_PROXY 环境。`http.open` 分配本 realm 句柄，`http.headers` 异步发送请求并返回元数据，`http.read` 由 ReadableStream 拉取响应块；没有提前汇总整个 SSE 响应。SDK 仍负责消息、reasoning_content、工具调用及 usage 解析。取消会唤醒在途的 headers/read，结束/失败删除句柄，realm 终止或退出会取消全部 HTTP 请求。请求体和解压后的累计响应体各限 16 MiB，每 realm 同时最多 128 个响应；连接超时 30 秒、每次模型调用总超时 600 秒。重定向由 JS 跟随，跨 origin 移除认证头；支持模型 JSON/字节请求和流式响应，未提供文件上传/通用浏览器平台。此轮只开放 API key 认证的 DeepSeek/MiMo，OAuth、Bedrock 和其他 provider 不在运行时接口中。

**agent** 的一次任务：
1. 等 context 加载完，从 `context.currentModel()` 取模型（provider、model、key 所在的环境变量）。
2. `context.openTask` 根据任务和运行事实构建对话。
3. 每一轮先让 context 检查预算并准备必要的摘要请求，再把准备好的上下文交给模型。只用 pi 的模型层：agent 在同一 JSC realm 导入 `ply:models`，调用预打包的 pi-ai `complete`；Rust 负责 HTTP/TLS 和响应块，模型请求不启动外部进程。pi 的会话、记忆、上下文文件和 compaction 不用，压缩策略由 Ply 自己管理。拿到响应或异常后，用 `fiber.record` 写 `model_call`（purpose=task 或 compaction），再处理停止原因、版本化追加和工具调用。
4. 回复里的工具调用逐个 `await plugins.x(...)`（互不依赖的一起跑），结果再追加给 context。
5. 默认不限制模型轮数，直到最终报告通过完整性校验；只有显式传入正整数 `max_turns` 时才启用对应上限。返回值带自身 `usage` 和整棵子树的 `total_usage`；任务失败或达到显式轮数上限时先用 `ply:checkpoints` 保存可重放的完整交互与下一轮位置，再写 `agent_usage` 汇总记录，移除用量状态并关闭内存对话。默认文件在宿主 ~/.ply/checkpoints，PLY_CHECKPOINT_DIR 可指定绝对目录；原子写入、0700/0600 权限、flock 排他恢复。已进入工具执行批次的 checkpoint 标记 tools_pending，禁止自动重放；只有完整结果入库后才可继续。恢复成功删除记录，恢复失败保留，历史 usage 单列不再次计入祖先。格式 4 保存权限、累计配额、报告状态及 context 格式 2 的内容块、固定前缀、工具快照和更新历史；明确拒绝旧格式，不自动迁移，恢复时只能收窄授权且不退还已消费名额。

**子 agent**：agent 在 `onLoad` 里执行 `context.registerTool("agent")`。模型调用这个工具时，沿用普通插件调用链，创建父 agent 的子 fiber，并再次运行 `onHandle`。每次调用新建独立对话，不自动复制父对话；任务背景和约束由父 agent 放进 `prompt`。父子 agent 共用挂载存储、工作目录及模型。`allowed_tools` 固定每个会话的工具定义集合，预算、usage 锚点签名、模型可见工具与执行前授权检查一致；子 agent 的白名单必须是父级子集。`max_delegation_depth` 主调用默认 1，子级继承后减 1，0 时不提供 agent 且入口拒绝进一步委派；更深层级须显式授权。父 agent 等待子 agent 的结果，将它作为工具结果追加到自己的对话；同轮多个子 agent 沿用 `Promise.all` 并发执行。递归使用内核已有的 fiber 深度限制（默认 16，所有工具调用都计入）；父子 agent 默认均无模型轮数上限，显式传入的 `max_turns` 仅限制对应 agent。

**直属子 agent 配额**：`max_children` 主调用默认 2，可由输入或 CLI `--agent-max-children` 配置；输入覆盖 CLI 默认。每个 agent 调用在开始模型请求前用 `fiber.limitChildren("agent", limit, restoredStarted)` 安装内核配额。`Kernel::start` 在 fiber 表锁内完成准入和计数，已耗尽时在 Spawn 前拒绝；计数不因成功、失败、取消而归还，替换及恢复也是新调用。配额是按父 fiber 累计的直属子数，普通工具和不同父 fiber 分开；子级只能收窄自己的上限。已安装的上限不能提高、已消费数不能减小；快照恢复把消费数恢复到新 fiber。`max_delegation_depth` 仍独立约束代数，0 配额或 0 代数都移除 agent 工具，模型轮数默认不限。

**报告完整性**：无工具调用的回复必须 stop、非空、以独立内部完成标记结尾，代码围栏闭合，非代码正文不含残留协议控制标记。`report_sections` 继承并合并父级及恢复要求，匹配 Markdown 标题并检查内容非空；配置缺省不强制特定标题。无效回复不追加到对话，仅加入校验原因，让同一 agent 使用保留的证据重新完成，不新增 agent；`length` 的最终文本也走修复，半截工具调用则直接失败、禁止执行。记录 rejected / validated 事件；要求和拒绝次数随快照保存，修复调用计入正常 usage，没有隐藏轮数上限。格式校验不保证事实正确。

**模型用量**：调用提供商前写 `model_call_started`，响应或异常后写相同 call_index 的 `model_call`。强制取消/VM 中断时可能只有 started，这表示用量未知，不能算零；终态事件与 started/end 配对用于审计。解释 pi-ai usage、统计 token 和计算命中率都在 agent 插件中，内核只提供通用 fiber 记录。`input_tokens = input + cacheRead + cacheWrite`，`total_tokens = input_tokens + output`，`reasoning` 是 output 的子集，不重复相加。命中率按累计 `cacheRead / input_tokens` 计算。无效、缺失或全零占位 usage 计入 `missing_calls`；汇总 `complete=false` 时 token 为已报告小计、命中率为 null；未报告推理量也是 null。每次任务或摘要模型调用都向自身和所有活跃祖先各累加一次，不从工具结果重复加总；每个 `complete` 算一次调用，不拆 SDK 内部重试。返回值 turns 仅统计任务循环轮数，usage.calls 包含摘要调用。`context_compaction` 单独记录压缩状态、前后估算规模、替换消息数及耗时；失败摘要的已上报用量也保留。

## 4. 同一个插件的多个 fiber

- 每个插件一个 realm（一个 JSC context group、一个线程）。同一个插件的多个 fiber 在这个 realm 里共享模块的闭包，在 `await` 处交替执行，和一个 node 进程里的多个请求一样。不同插件在不同线程上并行。
- 这是异步而不是多线程：JS 的堆只属于一个线程，没有引擎能让多个线程并行地读写同一份闭包对象。外部的 IO 和进程是真正并行的。
- Rust 桥接层在继续分配 JS 值前为调用参数建立 GC root，并在调用完成后通过 RAII 释放。存进 Rust `Vec` 的 JS 引用不会被 JSC 的栈扫描发现，必须显式保护；初始化参数和异步返回值也遵循这一生命周期。
- **父子关系怎么确定**：模块、prelude 与内嵌 pi-ai 的 async/await（含异步迭代器）由 OXC 转成 Promise 续体。注册 Promise reaction 时，原生可调用对象捕获消费方的调用 scope；Rust 将续体作为独立消息调度，并恢复该 scope 后执行。身份保存在原生私有数据中，不由插件传入 fiber ID。因此等待同一个共享 Promise 的两个 fiber，在 `await`、`then`、`Promise.all`、微任务、定时器及跨插件调用后仍各自归属正确。插件的同名变量不会覆盖编译器导入的 helper。
- **错误隔离**：普通异常、未处理 rejection、定时器回调异常、宿主异步任务 panic 都使所属调用失败；调用方可以捕获子 fiber 的失败，独立兄弟任务继续运行。CPU 超限只终止当前调用，已排入 Rust 队列的其他续体不被 JSC 的终止清空。事件 hook 的 panic 单独捕获，不中断其他 hook 或业务执行。
- **取消**：Rust `Kernel::cancel_fiber(id)` 请求取消，返回值只表示请求是否命中活动 fiber；以最终 `exit` 为完成依据。丢弃 Rust 调用 future 也会取消对应 JS 调用。调用级取消信号由 CPU watchdog 检查，并唤醒同步 fs/exec 的等待；同时清理所属定时器、HTTP、宿主异步任务与排队续体。取消父任务会通过被丢弃的等待 future 传播到子任务，不影响独立兄弟。共享 Promise 的其他消费者会收到拒绝，而不是永久挂起。
- **清理与恢复边界**：强制中断不依赖 JS generator 的 `finally`。第一方插件可通过 `ply:lifecycle.onDispose` 注册清理；运行时在独立调用中执行，受 CPU 预算及整批 2 秒墙钟期限约束。清理入口排入下一次 realm 调度，在失败调用离开 JS 栈后进入，避免 lazy thenable 的微任务沿用失败 scope。已排队的清理也计入 idle 判定。计时在排队前启动，期限同时保存在调用取消对象；超时直接设置标志并通知 watch，CPU watchdog 和同步 fs/exec 等待无需等 realm 消费取消消息。settle 再核对期限，迟到的成功不能覆盖超时。回调在执行前整批移出注册表；普通异常继续后续回调并汇总为失败，超时/取消中断剩余工作并释放宿主任务、定时器、HTTP 和续体，清理中禁止再次注册清理。清理成功 settle 也停止其 scope 并回收未 await 的定时器、宿主任务、HTTP 和续体，再发 Done；普通成功 onHandle/脚本不走这一额外回收，保留 Node 风格计时器语义。`FiberEvent::Cleanup` 用独立 id 记录 realm、owner、started 及一次 done/failed/timed_out/cancelled，可在所属 fiber 退出后观察到；不伪装成新的 agent 或业务 Spawn/Exit。JSC 中断有约 100 ms CPU 检查间隔，无法回滚已经发生的 IO 或强杀所有阻塞系统调用。agent 用它删除活动用量状态、关闭内存会话、释放已持有的恢复锁；已有快照保留，`tools_pending` 仍禁止自动重放。成功路径仍执行原有 `finally`。此清理机制不回滚已经发生的共享状态或磁盘写入，也不承诺为任意首次取消创建可恢复快照。
- **终态**：`exit.status` 为 `done`、`failed`、`cancelled` 或 `budget_exceeded`；预算退出带 `resource: cpu | memory`。每个已 spawn 的 fiber 恰有一次终态。模型 `stopReason: length` 含工具调用时在执行和追加前失败，保留最后完整交互的检查点；纯最终文本则由原 agent 重新生成完整报告，不把截断文字当完成。
- **边界**：同一插件仍共享闭包和一个 JS 线程；错误隔离不是共享状态的事务回滚。同步调用或长计算在返回/被中断前仍占用该 realm。内存阈值是全进程水位，不是每 fiber 的硬内存配额。归属保证覆盖 Ply 编译入口及可信内置插件；插件自行生成未经该入口转换的原生 async 代码或篡改 Promise 调度不在此保证内，插件代码仍是可信执行边界。

## 5. 分层与依赖

```
cli → kernel → core
         ↓       ↑
        jsc ─────┘
         ↓
         sh
         ↓
       libc → io → storage
```

- 运行依赖按图分层；jsc 复用 core 的 Cleanup 事件类型，其余上层需要的下层类型由相邻层重新导出（`ply-libc` 导出 IO 层的类型，`ply-jsc` 再导出给内核，内核再导出给 CLI）。
- 同一个概念只定义一次：
  - 插件描述、类型、`onLoad` 的挂载、fiber 和事件只在 `ply-core` 里定义，内核构造它们，hook 收到的、记录文件里写的都是它们的序列化；
  - `ExecResult` 交给 JS 的形式由它自己的 serde 定义决定。

| crate | 角色 | 关键点 |
|---|---|---|
| `ply-storage` | 存储后端 | 只认"以 `/` 为根的一棵树"。磁盘后端做路径和符号链接的逃逸防护，并报告自己的宿主根目录；KV 风格的后端只需实现 4 个方法。 |
| `ply-io` | IO 层 | 只有挂载的路径存在；可以随时 mount / unmount / swap；读写是挂载的属性。`exec` 在磁盘挂载上启动真实进程（见第 7 节）。 |
| `ply-libc` | 共享系统库 | 纯 Rust，不封装 C 的 libc：errno、path、fs、glob、time，所有文件操作都调用 `ply-io`。 |
| `ply-jsc` | JS 运行时，对齐 node | 内嵌系统 JavaScriptCore，TS 直接运行（oxc 擦除类型 + 自研模块加载）。安全和资源见第 6 节。 |
| `ply-core` | 描述层 | `PluginSpec`、`Type`（输入输出的结构化类型，可换算成 JSON Schema）、`Mounts`、`Fiber`、`Exit`、`FiberEvent`、`FiberHook`。不做 IO。 |
| `ply-kernel` | 内核 | 加载插件（读签名、建 realm、后台运行 `onLoad`、`ready`）、调度 fiber、维护每个 realm 的插件目录 / `ctx`、发出事件、`run_script`、默认插件。 |
| `ply-cli` | 入口 | `tools`、`call`、`mcp`、`run`、`commands` 和插件注册的顶层命令。 |
| `ply-sh` | shell | 模型 bash tool 的内部解释器，文件访问经 ply-libc/IO；包含 builtin 和工具集，不调用系统 shell。 |

## 6. JS 运行时

**宿主调用的校验**：prelude 和插件代码在同一个 realm 里，插件可以替换 prelude 用到的任何全局对象，所以 Rust 侧对每个宿主调用独立校验：
- 模块只能加载 ply 自己解析过的 id；
- 解析时先按路径判断是否在插件根目录内，在根目录外直接拒绝，不访问存储；
- `from` 必须是已加载的模块；
- settle 和定时器必须属于这个 realm。

prelude 在插件代码运行前保存了 `JSON`、`eval`、`Object` 的引用，并用无原型对象传参，只是为了插件改了全局对象时运行时本身不坏。

**资源**：
- 每次进入 JS 有 CPU 预算（默认 10 秒），进程内存有上限（默认 4 GiB），由 JSC 的 watchdog 每 100 ms CPU 时间检查一次。超出的那一轮被终止，只令所属调用以 budget_exceeded 结束，独立兄弟任务与 realm 继续可用；内存阈值仍是全进程水位。
- 未处理的 promise rejection 写到插件的 stderr。
- `idle` 等到没有定时器、没有在途 IO、没有在途调用；`ply run` 用它实现"定时器跑完才退出"。

**已知差异**：
- 不能直接 import npm 包；支持 `node:*`、内置 `ply:models` / `ply:checkpoints` 和插件目录下的相对路径。
- 内置模块：`fs`、`fs/promises`、`path`、`process`、`child_process`（`exec`、`execFile`、`execSync`、`execFileSync`、`spawnSync`，没有 `spawn`）、`buffer`、`events`、`util`、`os`、`timers`、`timers/promises`。没有 `net`、`http`、基于 fd 的 fs API 和流。
- TS 经过 oxc 重新生成代码，报错的行号对应生成后的代码，还没有 sourcemap。

**临时 realm 生命周期**：`Kernel::run_script` 用拥有者 guard 覆盖正常返回、失败和 future 被取消；回收时先从目录广播集合及 JSC 活跃表移除，发送关闭信号并在线程外等待退出。JSC 活跃表改为唯一 ID 的 HashMap，旧 ID 不复用，初始化失败/取消也会回滚。关闭信号可以打断同步 fs/exec 的 block_on，释放 async host jobs、定时器、HTTP 与 JSC 根；exec 的进程组和 stdin/stdout/stderr 任务有取消时的 RAII 清理。阻塞式脚本的 JS 代码仍受当前 CPU watchdog 预算约束。插件 realm 继续随 kernel 生命周期复用，临时脚本 realm 独立关闭。

## 7. 真实进程：`Io::exec`

- 工作目录必须在磁盘挂载上（真实进程只认宿主目录），内存和 SQLite 挂载对它不存在。
- 继承宿主的环境变量，再加上调用方给的。
- stdout / stderr 捕获，每路默认最多 1 MiB；stdin 由调用方给或 `/dev/null`。
- 进程跑在自己的进程组里；主进程退出或超时后整组被结束，后台残留的进程不会活过这次调用。
- 读输出的任务把内容写进共享缓冲区：即使管道因为写端被别的进程继承而迟迟不到 EOF，已经读到的输出也不会丢。macOS 上管道是先创建、再设置 close-on-exec 的，并发启动的进程可能在这个间隙继承写端。
- 进程能读写宿主上的什么，ply 不限制；需要限制的话在插件里做。

## 8. 入口

- `ply call PLUGIN JSON`：一次执行，输出 JSON；插件的 console 输出打到 stderr。
- `ply mcp`：MCP server（stdio，每行一条 JSON-RPC）。
  - 每个有 `onHandle` 的插件是一个 tool，`inputSchema` 由插件的输入类型换算成 JSON Schema；
  - 输入不是对象的插件，参数包成 `{ "input": ... }`；
  - 请求并发处理，插件失败返回 `isError: true`。
- `ply run FILE`：像 node 一样运行一个 TS/JS 文件，导入 `ply:plugins` 使用插件目录，`ctx` 可用。
- 加载完所有插件后 CLI 会先等 `ready`：注册、模型选择这些 `onLoad` 都完成了，才开始执行。
- 默认挂载当前目录（读写，挂在它自己的路径上），这样文件插件和 bash 看到的路径一致。

## 9. 测试

- `ply-storage/tests/conformance.rs`：同一套行为测试跑三种后端。
- `ply-io/tests/io_layer.rs`：只看到挂载、只读挂载、热切换存储、跨挂载改名、真实进程（磁盘挂载、stdin 和环境变量、超时和残留进程）。
- `ply-jsc/tests/node_outlet.rs`：
  - node API 和 IO 错误；
  - 隔离和调度；
  - 同一 realm 里多个 fiber 共享闭包，每次 `await plugins.x` 都对应到正确的 fiber；
  - 句柄惰性、只执行一次；
  - 模块根目录限制、篡改宿主调用；
  - 一个 realm 阻塞时其他 realm 照常；
  - 死循环和内存上限；
  - `child_process`。
- `ply-jsc` 单元测试：将参数保存在 Rust 堆上，构造期间强制执行完整 GC，验证传入 JS 的参数保持完整。
- `ply-kernel/tests/agent.rs`：agent 的循环，用同 realm 的 JS fixture 替换 `ply:models` 调用边界，不修改 PATH、不运行进程、不请求模型：模型和工具都来自 context、多轮、工具调用作为子 fiber、逐轮用量记录与加权汇总、子 agent 及失败调用的用量、不存在的工具、模型报错、出错时对话也会关闭、没有模型接入时报错；并覆盖同一 realm 内两个子 agent 的多轮并发、孙 agent、较大上下文及父子关系和用量不重计。
- `ply-kernel/tests/kernel.rs`：
  - 默认插件的描述（从签名读出）、哪些能被调用、行为；
  - 四个工具和模型都已注册到 context，对话的开关和工具输出截短；
  - `onLoad` 挂到 `ctx`（数据和函数）、先加载的插件在 `onLoad` 里等后加载的、后挂载覆盖先挂载、`ready` 之后等待不存在的插件立刻失败；
  - 插件目录的枚举和工具 schema；
  - fiber 的父子关系和记录；
  - 宿主唤起；
  - 同一插件并发执行时共享闭包；
  - 加载冲突和错误、嵌套上限；
  - JSONL 记录；
  - 脚本里等待插件。
- `ply-kernel/src/signature.rs`：找出 `onHandle` / `onLoad`，从 TS 签名和 JSDoc 读出描述和类型。


## 插件事件与内部 shell

事件在 `onLoad` 用 `ply:events.define/on` 声明、订阅，命名为 `<plugin>.<local>`；不加入原有 ctx 扁平覆盖逻辑。内核按原生 RealmId 核对声明者、订阅者和发布者；文件名/realm 标签相同不能冒充插件。注册只接受原生 onLoad 调用，事件发布在加载完成后进行。失败的 onLoad 会清除该插件的定义与订阅。

回调留在订阅插件的 JSC realm；发布方 await 内核分发，监听器按订阅顺序各自运行在新的子 fiber 中，parent 是发布 fiber，`fiber.event` 标明事件名。事件回调不调用插件 onHandle，不作为 agent 工具调用扣减配额。payload 经 JSON 复制，发布身份由内核填入；监听器返回值组成结果数组。异常或总分发期限到达会失败并取消在途回调，回调完成也回收其未等待的任务。`event:plugin` 记录发布，Spawn/Exit 记录每个监听器的归属与结果。

bash 插件在 onLoad 声明 `bash.beforeExecute`，每次 onHandle 先 await 事件，再通过原生 `ply:sh` 执行。第一版只有整个工具调用的 hook，不逐条拦截内部命令。任一结果为 false 或 allow:false 时不进入解释器；异常/超时同样失败。重定向和命令替换都在 hook 之后发生。脚本与 cwd 保持发布时的值，监听器不能通过修改自己收到的 payload 改写脚本。

内部 sh 已迁移到当前 IO，并重新加入 workspace；不恢复旧 IO gate/trace API。原生调用只提供该解释器的既有 builtin/utility，不给未知命令添加系统 shell 回退。脚本独立实例支持超时、协作取消、缓冲上限及语法深度保护；语法与后端边界见 crates/ply-sh/README.md。事件 API 和加载示例见 plugin-events.md。
