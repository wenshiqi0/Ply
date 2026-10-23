# wendy 设计

本文记录已经和作者对齐的设计决策。代码里每个 crate 的 `lib.rs` 顶部也有对应的 "Important decisions" 注释。

## 1. 管控对象：IO

机器上的副作用来自 IO，所以 wendy 管控的就是 IO。harness 要能知道整个上下文里发生的外部变动，至少在控制权交出去的时候（比如调用了外部程序）也能追踪。

- 插件（TS，跑在 JSC 沙箱里）的文件 IO 走 `wendy-io`：同一张挂载表、同一个闸门、同一份 trace。
- **起一个真实进程也是 IO**（`Io::exec`）。进程是黑盒，副作用只能经过通道出来，wendy 掌握它的所有通道：启动要过闸门；stdio 被捕获；文件写入被系统沙箱限制在读写挂载内，并在运行期间被观测；网络默认关闭；进程组在调用结束时被结束。
- **effect 不预先声明。** 一次操作改了什么，就是 IO 层看到它做了什么：每次 IO、以及它启动的进程造成的每处改动，都带着所属的 IPO 操作记进 trace。

## 2. 沙漏结构

```
                入口：wendy tools / call / mcp / run
     ┌──────────────────────────────────────────────┐
     │   插件 = 工具的封装（TS，可以很多）               │   上部：宽
     │   内核：每次工具调用是一次 IPO 操作                 │
     └───────────────────────┬──────────────────────┘
                      ┌──────┴──────┐
                      │ JSC（node） │                  瓶颈：插件只能从这里出去
                      └──────┬──────┘
     ┌───────────────────────┴──────────────────────┐
     │  wendy-libc：fs / path / glob / errno / time    │   下部：宽，模块化
     └───────────────────────┬──────────────────────┘
          wendy-io（唯一受控 IO：挂载、闸门、trace、exec）
             │                              │
   wendy-storage：磁盘 / 内存 / SQLite        系统进程（bash）：Seatbelt + FSEvents
```

依赖方向只能自上而下：

```
cli → kernel → core
         ↓
        jsc
         ↓
       libc → io → storage
                ↘ trace
```

## 3. 各层职责

| crate | 角色 | 关键点 |
|---|---|---|
| `wendy-storage` | 存储后端 | 只认"以 `/` 为根的一棵树"。磁盘后端做路径和符号链接的逃逸防护，并报告自己的宿主根目录（真实进程只能在这种存储上工作）；KV 风格的后端只需实现 4 个方法。 |
| `wendy-trace` | trace + 落盘 hook | 每个决策点发一条事件，带 verdict，拒绝和放行一样可见。`JsonlHook` 每条事件写一行并立即 flush；CLI 每次运行带一个 `run_id`。 |
| `wendy-io` | 唯一受控 IO（相当于内核的系统调用边界） | 只有挂载的路径存在；每次调用依次经过闸门、只读检查、执行、trace。`exec` 见第 6 节。actor 记录 runtime、会话、API 或程序名、所属 IPO 操作。 |
| `wendy-libc` | 共享系统库 | 纯 Rust，不封装 C 的 libc，所有文件操作都调用 `wendy-io`。 |
| `wendy-jsc` | 出口：对齐 node | 每个插件一个 realm、一个线程、一个 context group。TS 直接运行（oxc 擦除类型 + 自研模块加载）。安全模型见第 7 节。 |
| `wendy-core` | 描述层 | IPO 操作、阶段、插件和工具的描述；不做 IO，字段不强类型化。 |
| `wendy-kernel` | IPO 内核 | 加载插件、注册工具、运行工具调用、插件之间互相调用和唤起、审批操作。 |
| `wendy-cli` | 入口 | `tools`、`call`、`mcp`（stdio 上的 MCP server）、`run`。 |
| `wendy-sh` | 暂停 | 自研的对齐 bash 的 sh。目前工具直接用系统 bash（经 `exec`），源码保留在仓库里但不参与构建，也不再跟随其他 crate 的接口变化。 |

## 4. IPO、插件与工具（已实现）

- **插件就是工具的封装。** `definePlugin({ name, tools })`，每个工具有 `description`、`schema`（输入的 JSON Schema）和三个阶段：`input(raw, ctx)` → `process(input, ctx)` → `output(result, ctx)`，`input` 和 `output` 可省略。
- **每次工具调用是一次 IPO 操作**，有 id（`op-12`）和父操作。trace 里依次是 `ipo.start`（输入、父操作、深度）、三条 `ipo.phase`（由 realm 报告，Rust 校验操作属于这个 realm）、`ipo.end`（输出或错误、耗时）。
- 操作由内核并发调度。插件通过 `ctx.call(tool, input)` 调用别的工具并 `await`（子操作），通过 `ctx.wake(tool, input)` 主动唤起而不等待（`detached`）。嵌套深度有上限（默认 16）。
- 工具名全局唯一，后加载的插件和已有工具重名会加载失败。
- 插件代码从它自己的只读命名空间（`wendy-io`）读取，与工作区分开，trace 共用：加载代码也是一次被记录的读。
- **IO 归属到操作**：realm 里只有一个操作在进行时，它的 IO 自动带上这个操作的 id；`ctx.exec` / `ctx.call` 显式带上 id（Rust 校验它属于这个 realm）。多个操作并发时，直接用 `node:fs` 的 IO 只归属到插件，不猜操作。
- 内置工具是一个普通插件（`plugins/builtin/tools.ts`，编译进二进制）：`bash`、`read`、`edit`、`create`、`delete`。后四个走 `node:fs` → `wendy-io`；`bash` 走 `exec`，返回退出码、输出和它改动的文件。

## 5. 审批模型

`Policy` 的判定顺序：
1. 已记住的授权（之前回答过 "always"）；
2. 按顺序匹配路径规则（Allow / Deny / Ask）；
3. 按读写类型取默认值：读默认放行，写默认询问。起进程按写处理，路径是它的工作目录，审批者能看到完整的 argv。

结果为 Ask 时调用 `Approver`。**审批本身也是一次 IPO 操作**：`Kernel::approver` 包装宿主提供的审批方式（终端文字确认、弹窗、内部规则），input 是请求，output 是回答，父操作是发起这次 IO 的操作。等待回答期间，这次 IO 调用原地挂起；只有发起它的插件线程被挡住，其他插件照常运行。

## 6. 系统 bash：`Io::exec`

| 通道 | 做法 |
|---|---|
| 启动 | 过闸门（`OpKind::Exec`）；工作目录必须在磁盘挂载上；trace `exec.spawn`（argv、cwd、pid、网络策略、可写挂载） |
| 环境 | 只继承白名单（PATH、HOME、USER、LANG 等），其余由调用方显式传入；宿主里的密钥不会带进去 |
| stdio | stdin 由调用方给或 `/dev/null`；stdout / stderr 捕获，每路默认最多 1 MiB，trace 里保留前 8 KiB |
| 文件写入 | Seatbelt（`sandbox-exec`）只允许写读写磁盘挂载、每次调用的私有 `TMPDIR` 和系统的用户临时目录；嵌在读写挂载里的只读挂载仍然不可写 |
| 观测改动 | 运行期间用 FSEvents 监视读写挂载；进程结束后在私有临时目录写一个"栅栏"文件，flush 到栅栏事件出现为止（事件 id 全局有序，所以之前的事件都已送达）；wendy 自己的写入（`MarkSelf`）被排除。每处净改动记成 `io.write` / `io.mkdir` / `io.remove`，`observed: true`，actor 为 `exec` / 程序名 / 所属操作 |
| 网络 | 默认 `deny network*`；`--allow-net` 打开 |
| 生命周期 | 单独的进程组；主进程退出或超时后整组被 SIGKILL，后台残留进程不会活过这次调用 |

已知限制：
- 读不受限（它是系统 bash，需要读系统），只能用 `--deny-read` 挡住指定目录。
- Seatbelt profile 是"允许一切再收回写入和网络"，是护栏而不是完整的安全边界：进程仍然可以请其他已运行的程序代劳（Apple Events、launchd 任务）。
- 用户临时目录（`/var/folders/.../T`）可写且不观测，因为 macOS 的 `mktemp` 不认 `TMPDIR`。
- 改动的归属假设运行期间没有其他程序写同一批目录。看到的是净改动，看不到中间过程；在调用内创建又删除的文件不报告。
- 内存和 SQLite 挂载对真实进程不可见。
- 还没有流式进程（`child_process.spawn`）、交互式输入和长期运行的进程。
- 写入只能限制和记录，不能逐次事先拦截；要做到这一点需要把 `wendy-io` 暴露成真实文件系统（NFS 回环或 FSKit）。
- 网络还不是 `wendy-io` 里的一类 IO：现在只有开 / 关，开了之后看不到请求内容。

## 7. JS 出口

**安全模型**：prelude 和插件代码在同一个 realm 里，插件可以替换 prelude 用到的任何全局对象，所以 prelude 不是安全边界，Rust 侧对每个宿主调用独立校验：
- 模块只能加载 wendy 自己解析过的 id；解析时先按路径判断是否在插件根目录内，在根目录外直接拒绝，不访问存储（不泄露"文件是否存在"）；`from` 必须是已加载的模块。
- trace 里的 API 标签只能取每个 op 对应的固定列表（`fs.readFileSync`、`fs.promises.readFile` 等），同步调用不能冒充异步 API。
- settle、定时器、IPO 阶段、操作 id 都必须属于这个 realm。
- 不合法的调用被拒绝，并记一条 `js.violation`。prelude 另外在插件代码运行前保存了 `JSON`、`eval`、`Object` 的引用，并用无原型对象传参，只是为了插件改全局时运行时本身不坏。

**调度与资源**：
- 每个 realm 独立线程：同步 IO 等审批、长计算只挡住自己。
- 每次进入 JS 有 CPU 预算（默认 10 秒），进程内存有上限（默认 4 GiB），由 JSC 的 watchdog 每 100 ms CPU 时间检查一次。超出的那一轮被终止，这个 realm 里在途的调用全部失败，realm 继续可用；记 `js.terminated`。
- 未处理的 promise rejection 写到 stderr 并记 `js.unhandled_rejection`。
- `JsRuntime::idle` 等到没有定时器、没有在途 IO、没有在途调用；`wendy run` 用它实现"定时器跑完才退出"。

**已知差异**：
- 不能 import npm 包，只能 import `node:*`、`wendy` 和插件根目录下的相对路径。
- 内置模块：`fs`、`fs/promises`、`path`、`process`、`child_process`（`exec`、`execFile`、`execSync`、`execFileSync`、`spawnSync`，没有 `spawn`）、`buffer`、`events`、`util`、`os`、`timers`、`timers/promises`。没有 `net`、`http`、基于 fd 的 fs API 和流。
- TS 经过 oxc 重新生成代码，报错的行号对应生成后的代码，还没有 sourcemap。
- 工具输入只做插件自己写的检查，内核还不按 schema 校验。

## 8. 入口

- `wendy call TOOL JSON`：一次工具调用，输出 JSON；插件的 console 输出打到 stderr。
- `wendy mcp`：MCP server（stdio，每行一条 JSON-RPC），`tools/list` 和 `tools/call` 直接对应内核的工具和调用，请求并发处理；工具失败返回 `isError: true`。`--ask` 依赖 `/dev/tty`，在没有终端的 MCP 客户端里审批会被拒绝；以后可以改用 MCP 的 elicitation，让客户端来问用户。
- 默认挂载当前目录（读写，挂在它自己的路径上），这样文件工具和 bash 看到的路径一致。

## 9. 测试

- `wendy-storage/tests/conformance.rs`：同一套行为测试跑三种后端。
- `wendy-io/tests/io_layer.rs`：只看到挂载、只读、闸门、热切换。
- `wendy-io/tests/exec.rs`：进程只能写读写挂载、每处改动被观测并归属、闸门先于启动、网络默认关闭、超时和残留进程被结束、宿主环境变量不泄露。
- `wendy-jsc/tests/node_outlet.rs`：node API、trace 归属、隔离、调度；第一版沙箱里发现的两个逃逸（替换 `JSON.stringify` 加载根目录外的模块、伪造 trace 标签）的回归测试；直接发送被篡改的宿主调用；审批挡住一个 realm 时其他 realm 照常；死循环和内存上限；未处理 rejection；`child_process`。
- `wendy-kernel/tests/kernel.rs`：内置工具、IPO 事件和 IO 的操作归属、插件互相调用和唤起、嵌套上限、加载冲突、审批作为子操作。
