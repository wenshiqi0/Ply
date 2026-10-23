# wendy 设计

本文记录已经和作者对齐的设计决策。代码里每个 crate 的 `lib.rs` 顶部也有对应的 "Important decisions" 注释。

## 1. 插件就是一个函数

- 一个插件是一个目录，`main.ts`（或 `index.ts` / `main.js` / `index.js`）导出一个函数 `handle(input)`。插件之间相互独立，不聚合、不声明依赖、不注册。
- 插件声明得越少越好：
  - **名字**是目录名；
  - **描述**是 `handle` 上的 JSDoc；
  - **输入和输出**的描述就是 `handle` 的参数类型和返回类型（`Promise<T>` 取 `T`）。wendy 加载插件时用 oxc 从签名里读出来，同一文件里的 `interface` / `type` 会展开，读不懂的部分记为 `unknown`。
- 参数校验由插件自己做，内核不校验。权限这类控制也放在插件里，内核和 IO 层不做。

## 2. fiber

- 插件的每一次执行是一个 fiber，由内核调度。
- **插件之间只等待，不调用。** 插件代码里的全局 `tools` 按名字给出别的插件：`tools.read(input)` 是一个惰性的 thenable，持有它什么都不会发生，`await`（`then`）的那一刻就是调用，内核把它作为当前 fiber 的子 fiber 来跑。同一个句柄 await 两次只执行一次，和 Promise 一样。并发直接用 `Promise.all`。
- **主动唤起只来自宿主**（`Kernel::wake`），插件之间不互相唤起。
- 嵌套深度有上限（默认 16），防止插件无限等待自己。
- `tools` 可以枚举：`Object.keys(tools)` 是已加载的插件，每一项带 `description`、`parameters`（作为工具时参数的 JSON Schema）和 `wrapsInput`。这份清单由内核在加载插件时推送给每个 realm；schema 由 core 的 `PluginSpec::tool_schema` 统一生成，MCP 也用它。工具参数总是对象，输入不是对象的插件，参数包成 `{ "input": ... }`。
- **记录的是 fiber**：每个 fiber 的 spawn（id、父 fiber、插件、输入）和 exit（输出或错误、耗时）是一个 `FiberEvent`，交给注册的 `FiberHook`。CLI 的 `--record FILE` 把它们写成 JSON lines。IO 层不做任何跟踪。

## 3. 默认插件

agent、bash、create、edit、remove，和第三方插件写法完全一样，打包进二进制。

**agent** 用语言模型完成一个任务，其他插件就是它的工具：
- 和 dsh 一样，只用 pi 的模型层：每一轮由一个很短的 node 进程加载 pi-ai（从已安装的 pi 里找，或用 `WENDY_PI_AI` 指定），调用 `complete`，把模型的回复交回来。pi 的会话、记忆、上下文文件、compaction 都不用。
- 对话循环在插件里：发送上下文、执行回复里的工具调用（`await tools.x(...)`，互不依赖的调用一起跑）、把结果加回上下文，直到模型不再调用工具或达到轮数上限。对话只在这一次任务里存在。
- 模型写成 `provider/model-id`，默认 `deepseek/deepseek-v4-flash`；鉴权就是 pi-ai 认的环境变量。
- agent 不把自己列为工具。

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
  - 插件描述、类型、fiber 和事件只在 `wendy-core` 里定义，内核构造它们，hook 收到的、记录文件里写的都是它们的序列化；
  - `ExecResult` 交给 JS 的形式由它自己的 serde 定义决定。

| crate | 角色 | 关键点 |
|---|---|---|
| `wendy-storage` | 存储后端 | 只认"以 `/` 为根的一棵树"。磁盘后端做路径和符号链接的逃逸防护，并报告自己的宿主根目录；KV 风格的后端只需实现 4 个方法。 |
| `wendy-io` | IO 层 | 只有挂载的路径存在；可以随时 mount / unmount / swap；读写是挂载的属性。`exec` 在磁盘挂载上启动真实进程（见第 7 节）。 |
| `wendy-libc` | 共享系统库 | 纯 Rust，不封装 C 的 libc：errno、path、fs、glob、time，所有文件操作都调用 `wendy-io`。 |
| `wendy-jsc` | JS 运行时，对齐 node | 内嵌系统 JavaScriptCore，TS 直接运行（oxc 擦除类型 + 自研模块加载）。安全和资源见第 6 节。 |
| `wendy-core` | 描述层 | `PluginSpec`、`Type`（输入输出的结构化类型，可换算成 JSON Schema）、`Fiber`、`Exit`、`FiberEvent`、`FiberHook`。不做 IO。 |
| `wendy-kernel` | 内核 | 加载插件（读签名、建 realm）、调度 fiber、发出事件、`run_script`、默认插件。 |
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
  - 每个插件是一个 tool，`inputSchema` 由插件的输入类型换算成 JSON Schema；
  - 输入不是对象的插件，参数包成 `{ "input": ... }`；
  - 请求并发处理，插件失败返回 `isError: true`。
- `wendy run FILE`：像 node 一样运行一个 TS/JS 文件，`tools` 可用。
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
- `wendy-kernel/tests/agent.rs`：agent 的对话循环，用一个假的 `node` 代替模型调用：多轮、工具调用作为子 fiber、不存在的工具、模型报错。
- `wendy-kernel/tests/kernel.rs`：
  - 默认插件的描述（从签名读出）和行为；
  - `tools` 的枚举和工具 schema；
  - fiber 的父子关系和记录；
  - 宿主唤起；
  - 同一插件并发执行时共享闭包；
  - 加载冲突和错误、嵌套上限；
  - JSONL 记录；
  - 脚本里等待插件。
- `wendy-kernel/src/signature.rs`：从 TS 签名和 JSDoc 读出描述和类型。
