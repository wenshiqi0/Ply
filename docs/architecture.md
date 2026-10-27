# 架构

Rust 是主核心：内核加载插件、调度调用、提供 IO 并记录发生了什么；插件是运行在内嵌 JavaScriptCore 里的 TypeScript。设计细节见 [RFC 0001](rfcs/0001-runtime.md)，当前契约见 [SPEC](../SPEC.md)。

## crate

每个 crate 只依赖相邻的下一层。

| crate | 内容 |
|---|---|
| `ply-core` | 描述层：插件、输入输出类型、`onLoad` 的挂载、fiber、fiber 事件和 hook。类型只在这里定义一次 |
| `ply-kernel` | 加载插件（读出签名与具名导入，按依赖后台运行 `onLoad`）、调度 fiber、命令注册表、发出事件、`ready` / `close`、内置插件 |
| `ply-jsc` | 内嵌 JavaScriptCore，直接运行 TS；每个插件一个 realm、一个线程；Node 风格 API、ply 模块，以及调用归属与取消清理的续体调度 |
| `ply-io` | 挂载表（mount / unmount / swap，只有挂载的路径存在）；在磁盘挂载上启动真实进程 |
| `ply-storage` | 磁盘 / 内存 / SQLite 后端，同一套行为测试同时覆盖三种 |
| `ply-sh` | 内部 shell 解释器及工具集，执行模型 bash 工具的脚本，见 [ply-sh](../crates/ply-sh/README.md) |
| `ply-libc` | errno / path / fs / glob / time |
| `ply-cli` | tools / call / run / mcp / tui / commands，以及插件注册的顶层命令 |

## fiber：内核的调度单位

每次 `onHandle`、事件回调或命令的运行是内核调度的一个 fiber。插件代码看不到 fiber：它们之间的关系只通过输入、返回值和持有的值建立（[RFC 0012](rfcs/0012-capabilities-not-scheduling.md)）。

- **realm 与线程**：每个插件一个 JSC realm、一个线程。同一插件的 fiber 共享模块状态，在 `await` 处交替执行，像一个 node 进程里的多个请求；不同插件在不同线程上并行。
- **调用归属**：OXC 把 async/await 转成 Promise 续体；注册 Promise reaction 时，原生对象捕获消费方的调用，Rust 按这个归属调度续体。因此多个调用等待同一个共享 Promise 后仍各自归属正确，父子关系不会混淆。
- **隔离**：普通异常、未处理的 rejection、定时器回调异常、宿主任务 panic 都只让所属调用失败；每次进入 JS 有 CPU 预算，进程有内存水位，超限只终止当前调用。同一插件仍共享闭包和线程，错误隔离不回滚已经发生的共享状态或 IO。
- **嵌套**：调用可以嵌套，深度默认上限 16。
- **加载顺序**：插件的 `onLoad` 在它具名导入的插件完成 `onLoad` 后才开始；`ready` 等待所有 `onLoad`，之后才开始服务。

## 取消与清理

宿主取消、父调用取消、事件分发超时、CPU 或内存超限都会中断一个调用。

1. **立即结束**：调用的 `exit` 立即记录为 `cancelled` 或 `budget_exceeded`，父调用的取消会传播到它等待的子调用，独立的兄弟调用不受影响。
2. **有界清理**：
   - 它等待的宿主操作和不是它自己创建的 Promise 以 `AbortError`（`code: "ERR_PLY_CANCELLED"`）拒绝，它自己创建的叶子 Promise 也被拒绝。
   - 每个在取消前已存在的 async 帧恰好收到一次 `AbortError`，因此 `catch`/`finally` 照常运行，`finally` 里的 `await` 也正常工作。
   - 清理期间可以调用别的插件的方法、做宿主 IO，但不能发起新的子调用。
3. **回收**：整段清理有 2 秒墙钟期限；结束或超时后回收所属的定时器、HTTP、宿主等待和排队续体。

被强制终止的那段同步代码里的 `finally` 不会执行；已经发生的写入不会回滚。普通成功的调用保留 Node 风格的定时器行为。

宿主关闭内核（`Kernel::close`，CLI 退出前调用）时，先取消所有在途调用并等待它们清理，再按加载完成的逆序调用各插件导出的 `onDispose`，然后关闭 realm。

`--record` 的 `cleanup` 事件包含清理 `id`、`realm`、所属 fiber `owner`（`onDispose` 为空）、`status` 和可选的 `error`；状态为 `started` 加一次 `done` / `failed` / `timed_out` / `cancelled`，可能出现在对应调用的 `exit` 之后。

## 边界

- 插件代码是可信执行边界；模型 bash 工具的文件访问通过内部 sh 和挂载 IO，其他插件直接启动的宿主子进程不是 OS 沙箱。
- 内存阈值是全进程水位，不是每个调用的硬配额。
- 上下文 token 是保护性估算；报告完整性校验只验证格式和完成状态，不验证结论真伪。
