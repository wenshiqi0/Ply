# wendy 设计

本文记录已经和作者对齐的设计决策。代码里每个 crate 的 `lib.rs` 顶部也有对应的 "Important decisions" 注释。

## 1. 插件：两个钩子

- 一个插件是一个目录，`main.ts`（或 `index.ts` / `main.js` / `index.js`）用具名导出提供最多两个钩子。插件之间相互独立，不聚合、不声明依赖。
  - **`onHandle(input)`**：每次调用运行一次（一个 fiber）。
  - **`onLoad()`**：加载时运行一次，返回的东西挂到全局 `ctx` 上。
- 插件声明得越少越好：
  - **名字**是目录名；
  - **描述**是 `onHandle` 上的 JSDoc（没有 `onHandle` 时取 `onLoad` 上的）；
  - **输入和输出**的描述就是 `onHandle` 的参数类型和返回类型（`Promise<T>` 取 `T`）。wendy 加载插件时用 oxc 从签名里读出来，同一文件里的 `interface` / `type` 会展开，读不懂的部分记为 `unknown`。
- 没有 `onHandle` 的插件不能被调用，也就不是工具（不出现在 MCP 的 tools/list 里）。
- 参数校验由插件自己做，内核不校验。权限这类控制也放在插件里，内核和 IO 层不做。

## 2. fiber、`tools` 和 `ctx`

- **`tools.<name>`**：每个已加载的插件。
  - `tools.bash(input)` 是一个惰性的 thenable：持有它什么都不会发生，`await`（`then`）的那一刻就是调用 `onHandle`，内核把它作为当前 fiber 的子 fiber 来跑。同一个句柄 await 两次只执行一次，和 Promise 一样。并发直接用 `Promise.all`。
  - `tools.<name>.loaded` 是单独挂着的一个 promise，在该插件的 `onLoad` 返回时 resolve。
  - `description`，以及有 `onHandle` 时的 `parameters`（作为工具时参数的 JSON Schema）和 `wrapsInput`。schema 由 core 的 `PluginSpec::tool_schema` 统一生成，MCP 也用它。工具参数总是对象，输入不是对象的插件，参数包成 `{ "input": ... }`。
  - `Object.keys(tools)` 列出所有插件。
- **`ctx`**：所有插件的 `onLoad` 返回的成员，平铺在一起，不按插件名分隔。
  - 数据成员是 `onLoad` 返回时的一份拷贝；函数成员在挂载它的插件的 realm 里执行，所以调用总是返回 promise（跨线程只能如此）。
  - 同名成员，后挂载的覆盖先挂载的（按 `onLoad` 完成的先后），这是有意保留的覆盖方式。
- **加载**：
  - 内核加载插件时读签名、建 realm、执行模块，然后在后台运行 `onLoad`。所以 `onLoad` 里可以 `await tools.xxx.loaded` 等别的插件，不受加载顺序影响。
  - 宿主用 `Kernel::ready` 等所有 `onLoad` 结束。在那之后，等待一个不存在的插件会立刻失败，而不是一直挂着。
  - 调用一个还在加载的插件，会先等它加载完。
- **主动唤起只来自宿主**（`Kernel::wake`），插件之间不互相唤起。
- 嵌套深度有上限（默认 16），防止插件无限等待自己。
- **记录的是 fiber**：每个 fiber 的 spawn（id、父 fiber、插件、输入）、record（运行中主动记录的数据）和 exit（输出或错误、耗时）是一个 `FiberEvent`，交给注册的 `FiberHook`。CLI 的 `--record FILE` 把它们逐条写成 JSON lines 并 flush。`ctx` 上的函数调用不是 fiber，不自动记录。IO 层不做任何跟踪。
- **运行中的 fiber**：`await fiber.info()` 返回当前 id、parent 和从近到远的 ancestors；`await fiber.record(data)` 发出这个 fiber 的结构化记录。身份由内核从调用方取得，插件不传 id；两个方法只能在运行中的 fiber 内使用。内核不解释 data 的业务含义。

## 3. 默认插件

agent、bash、context、create、deepseek、edit、read、remove，和第三方插件写法完全一样，打包进二进制。

| 插件 | 钩子 | 职责 |
|---|---|---|
| context | onLoad | 管理模型看到的东西：注册进来的工具、要用的模型、进行中的对话。都存在模块闭包里，不持久化 |
| deepseek | onLoad | 本机的 DeepSeek 接入：只提供上下文集成，告诉 context 用哪个模型、key 在哪个环境变量 |
| bash、create、edit、read、remove | onHandle、onLoad | 各自完成一件事，并在 `onLoad` 里把自己注册到 context（`ctx.registerTool`） |
| agent | onHandle、onLoad | 用语言模型完成一个任务，并注册自己，允许模型通过同一工具开启子 agent |

**read** 通过 `node:fs/promises` 读取挂载存储里的 UTF-8 文本，不启动 shell。支持 `path`、从 1 开始的 `offset` 和 `limit`（默认 200 行）；返回原始文本和分页元数据，保留换行符。只限制返回的行范围，底层仍一次读取整个文件。agent 提示模型优先用它读取文件。

**context** 挂到 `ctx` 上的函数：`registerTool(name)`、`useModel(profile)`、`currentModel()`、`openConversation(system)`、`appendMessages(id, messages)`（返回模型此刻应看到的上下文：系统提示、消息、已注册的工具）、`closeConversation(id)`。第一版的上下文管理只做一件事：过长的工具输出只保留头尾（上限 20000 字符）。以后的上下文策略都只改这一个插件。

**agent** 的一次任务：
1. 等 context 加载完，从 `ctx.currentModel()` 取模型（provider、model、key 所在的环境变量）。
2. `ctx.openConversation` 开一个对话。
3. 每一轮把 context 给出的上下文交给模型。和 dsh 一样，只用 pi 的模型层：由一个很短的 node 进程加载 pi-ai（从已安装的 pi 里找，或用 `WENDY_PI_AI` 指定）并调用 `complete`；pi 的会话、记忆、上下文文件、compaction 都不用。拿到响应或调用异常后，用 `fiber.record` 写本轮 `model_call`，再处理停止原因和工具调用。
4. 回复里的工具调用逐个 `await tools.x(...)`（互不依赖的一起跑），结果再追加给 context。
5. 直到模型不再调用工具或达到轮数上限。返回值带自身 `usage` 和整棵子树的 `total_usage`；结束时（包括模型报错和达到轮数上限）先写 `agent_usage` 汇总记录，再移除用量状态并关闭对话。

**子 agent**：agent 在 `onLoad` 里执行 `ctx.registerTool("agent")`。模型调用这个工具时，沿用普通插件调用链，创建父 agent 的子 fiber，并再次运行 `onHandle`。每次调用新建独立对话，不自动复制父对话；任务背景和约束由父 agent 放进 `prompt`。父子 agent 共用挂载存储、工作目录，以及 context 提供的模型和工具。父 agent 等待子 agent 的结果，将它作为工具结果追加到自己的对话；同轮多个子 agent 沿用 `Promise.all` 并发执行。递归使用内核已有的 fiber 深度限制（默认 16，所有工具调用都计入），每个 agent 的 `max_turns` 单独生效。

**模型用量**：解释 pi-ai usage、统计 token 和计算命中率都在 agent 插件中，内核只提供通用 fiber 记录。`input_tokens = input + cacheRead + cacheWrite`，`total_tokens = input_tokens + output`，`reasoning` 是 output 的子集，不重复相加。命中率按累计 `cacheRead / input_tokens` 计算，范围 0–1。无效、缺失或全零占位 usage 计入 `missing_calls`；汇总的 `complete` 为 false 时，token 字段是已报告小计，整体命中率为 null；未报告的推理量也是 null。运行中的 agent 按内核 ancestry 关联，在每次模型调用后向自身和所有活跃祖先各累加一次，因此已报告用量在子 agent 失败后仍保留，且不从工具返回结果重复计数。每个 `complete` 算一轮，不拆 SDK 内部重试。

## 4. 同一个插件的多个 fiber

- 每个插件一个 realm（一个 JSC context group、一个线程）。同一个插件的多个 fiber 在这个 realm 里共享模块的闭包，在 `await` 处交替执行，和一个 node 进程里的多个请求一样。不同插件在不同线程上并行。
- 这是异步而不是多线程：JS 的堆只属于一个线程，没有引擎能让多个线程并行地读写同一份闭包对象。外部的 IO 和进程是真正并行的。
- **父子关系怎么确定**：每次进入 JS 都打上 fiber 标记：
  - fiber 开始执行时，这一轮标记为该 fiber；
  - 这一轮里发起的异步 IO 和定时器都记住这个 fiber，它们完成或触发时的那一轮沿用同一个标记；
  - `tools.x(...)` 被 `then` 时，当前标记就是父 fiber。

  唯一的偏差：fiber B 去 `await` 一个由 fiber A 创建的 promise（比如闭包里缓存的 promise）时，B 后续的代码在 A 的标记下继续执行，此后 B 再等待的插件会被记成 A 的子 fiber。要完全准确需要引擎提供异步上下文，JSC 没有。

## 5. 分层与依赖

```
cli → kernel → core
         ↓
        jsc
         ↓
       libc → io → storage
```

- 依赖严格按层：每个 crate 只依赖箭头指向的那一层，上层需要的下层类型由相邻的下层重新导出（`wendy-libc` 导出 IO 层的类型，`wendy-jsc` 再导出给内核，内核再导出给 CLI）。
- 同一个概念只定义一次：
  - 插件描述、类型、`onLoad` 的挂载、fiber 和事件只在 `wendy-core` 里定义，内核构造它们，hook 收到的、记录文件里写的都是它们的序列化；
  - `ExecResult` 交给 JS 的形式由它自己的 serde 定义决定。

| crate | 角色 | 关键点 |
|---|---|---|
| `wendy-storage` | 存储后端 | 只认"以 `/` 为根的一棵树"。磁盘后端做路径和符号链接的逃逸防护，并报告自己的宿主根目录；KV 风格的后端只需实现 4 个方法。 |
| `wendy-io` | IO 层 | 只有挂载的路径存在；可以随时 mount / unmount / swap；读写是挂载的属性。`exec` 在磁盘挂载上启动真实进程（见第 7 节）。 |
| `wendy-libc` | 共享系统库 | 纯 Rust，不封装 C 的 libc：errno、path、fs、glob、time，所有文件操作都调用 `wendy-io`。 |
| `wendy-jsc` | JS 运行时，对齐 node | 内嵌系统 JavaScriptCore，TS 直接运行（oxc 擦除类型 + 自研模块加载）。安全和资源见第 6 节。 |
| `wendy-core` | 描述层 | `PluginSpec`、`Type`（输入输出的结构化类型，可换算成 JSON Schema）、`Mounts`、`Fiber`、`Exit`、`FiberEvent`、`FiberHook`。不做 IO。 |
| `wendy-kernel` | 内核 | 加载插件（读签名、建 realm、后台运行 `onLoad`、`ready`）、调度 fiber、维护每个 realm 的 `tools` / `ctx`、发出事件、`run_script`、默认插件。 |
| `wendy-cli` | 入口 | `tools`、`call`、`mcp`、`run`。 |
| `wendy-sh` | 暂停 | 自研的对齐 bash 的 sh，源码保留在仓库里，不参与构建，也不跟随其他 crate 的接口变化。 |

## 6. JS 运行时

**宿主调用的校验**：prelude 和插件代码在同一个 realm 里，插件可以替换 prelude 用到的任何全局对象，所以 Rust 侧对每个宿主调用独立校验：
- 模块只能加载 wendy 自己解析过的 id；
- 解析时先按路径判断是否在插件根目录内，在根目录外直接拒绝，不访问存储；
- `from` 必须是已加载的模块；
- settle 和定时器必须属于这个 realm。

prelude 在插件代码运行前保存了 `JSON`、`eval`、`Object` 的引用，并用无原型对象传参，只是为了插件改了全局对象时运行时本身不坏。

**资源**：
- 每次进入 JS 有 CPU 预算（默认 10 秒），进程内存有上限（默认 4 GiB），由 JSC 的 watchdog 每 100 ms CPU 时间检查一次。超出的那一轮被终止，这个 realm 里在途的 fiber 全部失败，realm 继续可用。
- 未处理的 promise rejection 写到插件的 stderr。
- `idle` 等到没有定时器、没有在途 IO、没有在途调用；`wendy run` 用它实现"定时器跑完才退出"。

**已知差异**：
- 不能 import npm 包，只能 import `node:*` 和插件目录下的相对路径。
- 内置模块：`fs`、`fs/promises`、`path`、`process`、`child_process`（`exec`、`execFile`、`execSync`、`execFileSync`、`spawnSync`，没有 `spawn`）、`buffer`、`events`、`util`、`os`、`timers`、`timers/promises`。没有 `net`、`http`、基于 fd 的 fs API 和流。
- TS 经过 oxc 重新生成代码，报错的行号对应生成后的代码，还没有 sourcemap。

## 7. 真实进程：`Io::exec`

- 工作目录必须在磁盘挂载上（真实进程只认宿主目录），内存和 SQLite 挂载对它不存在。
- 继承宿主的环境变量，再加上调用方给的。
- stdout / stderr 捕获，每路默认最多 1 MiB；stdin 由调用方给或 `/dev/null`。
- 进程跑在自己的进程组里；主进程退出或超时后整组被结束，后台残留的进程不会活过这次调用。
- 读输出的任务把内容写进共享缓冲区：即使管道因为写端被别的进程继承而迟迟不到 EOF，已经读到的输出也不会丢。macOS 上管道是先创建、再设置 close-on-exec 的，并发启动的进程可能在这个间隙继承写端。
- 进程能读写宿主上的什么，wendy 不限制；需要限制的话在插件里做。

## 8. 入口

- `wendy call PLUGIN JSON`：一次执行，输出 JSON；插件的 console 输出打到 stderr。
- `wendy mcp`：MCP server（stdio，每行一条 JSON-RPC）。
  - 每个有 `onHandle` 的插件是一个 tool，`inputSchema` 由插件的输入类型换算成 JSON Schema；
  - 输入不是对象的插件，参数包成 `{ "input": ... }`；
  - 请求并发处理，插件失败返回 `isError: true`。
- `wendy run FILE`：像 node 一样运行一个 TS/JS 文件，`tools` 和 `ctx` 可用。
- 加载完所有插件后 CLI 会先等 `ready`：注册、模型选择这些 `onLoad` 都完成了，才开始执行。
- 默认挂载当前目录（读写，挂在它自己的路径上），这样文件插件和 bash 看到的路径一致。

## 9. 测试

- `wendy-storage/tests/conformance.rs`：同一套行为测试跑三种后端。
- `wendy-io/tests/io_layer.rs`：只看到挂载、只读挂载、热切换存储、跨挂载改名、真实进程（磁盘挂载、stdin 和环境变量、超时和残留进程）。
- `wendy-jsc/tests/node_outlet.rs`：
  - node API 和 IO 错误；
  - 隔离和调度；
  - 同一 realm 里多个 fiber 共享闭包，每次 `await tools.x` 都对应到正确的 fiber；
  - 句柄惰性、只执行一次；
  - 模块根目录限制、篡改宿主调用；
  - 一个 realm 阻塞时其他 realm 照常；
  - 死循环和内存上限；
  - `child_process`。
- `wendy-kernel/tests/agent.rs`：agent 的循环，用一个假的 `node` 代替模型调用：模型和工具都来自 context、多轮、工具调用作为子 fiber、逐轮用量记录与加权汇总、子 agent 及失败调用的用量、不存在的工具、模型报错、出错时对话也会关闭、没有模型接入时报错。
- `wendy-kernel/tests/kernel.rs`：
  - 默认插件的描述（从签名读出）、哪些能被调用、行为；
  - 四个工具和模型都已注册到 context，对话的开关和工具输出截短；
  - `onLoad` 挂到 `ctx`（数据和函数）、先加载的插件在 `onLoad` 里等后加载的、后挂载覆盖先挂载、`ready` 之后等待不存在的插件立刻失败；
  - `tools` 的枚举和工具 schema；
  - fiber 的父子关系和记录；
  - 宿主唤起；
  - 同一插件并发执行时共享闭包；
  - 加载冲突和错误、嵌套上限；
  - JSONL 记录；
  - 脚本里等待插件。
- `wendy-kernel/src/signature.rs`：找出 `onHandle` / `onLoad`，从 TS 签名和 JSDoc 读出描述和类型。
