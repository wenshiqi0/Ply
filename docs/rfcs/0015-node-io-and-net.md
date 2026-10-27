# RFC 0015: 原生句柄与 Node 的网络、进程 API

状态：分阶段实施。第一阶段已实现（2026-10-27），第二至五阶段待实施。
关联规范：SPEC-PLUGINS、SPEC-TOOLS。

## 问题与证据

以 Node 24.14.0 为基准逐项比对（[RFC 0014](0014-node-compat-suite.md)）：

- Node 有 58 个内置模块，Ply 只有 12 个。
- 已有的模块也不完整：`fs` 103 个导出里只有 36 个，`process` 66 里有 23，`util` 31 里有 7，`events` 15 里只有 `EventEmitter` 本身，`Buffer` 原型缺 85 个方法；`child_process.spawn` 调用即抛错，`process.on('exit')` 不会触发。
- 没有 `net`、`http`、`http2`，插件不能开 socket，依赖它们的 npm 包加载即失败。
- 没有 `stream`。`net.Socket` 本身是 Duplex 流，http 建立在 net 之上，`spawn` 的 stdio 和 `process.stdin` 也是流，所以 stream 是这些能力共同的前提。
- 现有的宿主调用是"字符串 op + JSON 参数"，不适合 socket 这类长期存在、双向、带回调的资源。

## 方案

### 1. 分层：Rust 注入句柄，JS 薄包装

照 Node 自己的划分：

- **Rust：资源与原生编解码**，对应 Node 的 C++ 层。
  - 用 JSC 的 C API 定义原生句柄类，把实例作为真实的 JS 对象交给运行时，每个类带方法和回调：
    - `TCP`：连接、监听、接受、读写、半关闭、关闭，含 Unix socket；
    - `TLS`：rustls 客户端与服务端，使用系统根证书；
    - `Process`：流式 stdio、kill、退出状态；
    - `Signal`；
    - `HTTPParser`：基于 httparse，承担 llhttp 的角色；
    - `Http2Session`：基于 h2，承担 nghttp2 的角色。
  - 句柄只交给运行时自己的 JS，不作为全局或 `ply:` 模块暴露给插件。
- **JS：Node 的 API 对象**，对应 Node 的 `lib/`。
  - `stream`、`net`、`tls`、`http`、`https`、`http2`、`child_process`、`process` 的事件与 stdin、`timers`、`os` 由运行时自己的 JS 实现，包在句柄外，继承 `stream` 和 `EventEmitter`，可以被子类化，也可以给原型打补丁。
  - 运行时自己编写，不移植 Node 的 lib，也不用 npm polyfill；语义以对照用例为准。
  - 这些模块用 TypeScript 写成 ES 模块，放在 `crates/ply-jsc/src/node/`：
    - build.rs 收集目录中的文件生成模块表，内置的 OXC 每个进程转译一次，各 realm 在首次导入时求值；
    - 模块之间用相对路径导入，公开的内置模块写作 `node:x`，prelude 的内部函数以 `ply:runtime` 导入，插件代码解析不到这个名字；
    - 导出用 `@types/node` 的类型约束（类型注解、`satisfies`、`implements`），签名与 Node 的声明不符时 `tsc` 报错；
    - 遵守 250 行规则。
  - prelude.js、web.js、async.js 的迁移另作一步。
- 句柄回调在所属调用的作用域中运行，与定时器、HTTP 请求一样遵循 RFC 0012 的归属与取消：调用创建的句柄随调用回收，onLoad 中创建的句柄随 realm 回收。

### 2. 进程身份与资源

- `process.pid`、`ppid`、`getuid`/`getgid`、`execPath` 报告 ply 进程的真实值，与子进程的真实 pid 一致。`process.env` 与 `HOME` 仍是精选的，密钥规则（RFC 0013）不变。
- `process.stdin` 只有 `ply run` 脚本读到真实输入。MCP 和 TUI 模式下 stdin 属于宿主协议和终端，插件得到一个立即结束的空流。
- 插件是可信的：socket、子进程和信号不经审批。每个 realm 有打开句柄的数量上限，与现有的 HTTP 请求上限一样。
- 模型的 bash 工具不受影响，仍走 Ply sh。

### 3. 五个阶段

每个阶段在 `tests/node/` 增加对应类别的用例，由 Node 24.14.0 录制期望值，独立验证和提交。

1. **基础**：
   - `stream`：Readable、Writable、Duplex、Transform、PassThrough、`pipeline`、`finished`、`Readable.from`、异步迭代，以及 `stream/promises`；
   - `string_decoder`；
   - `Buffer` 的读写、比较与复制方法，`buffer` 模块的 `atob`、`btoa`、`constants`；
   - `events`：模块级的 `once`、`on`、`getEventListeners`、`errorMonitor`，`newListener`/`removeListener` 事件，`prependListener`、`eventNames`、`setMaxListeners` 等；
   - `util` 的常用函数，`util/types`；
   - 全局 `global`、`atob`、`btoa`。
   这一阶段只需要 JS，不新增句柄。
2. **进程与系统**：`Process`、`Signal` 句柄；`child_process.spawn`；`exec` 系列改建在 `spawn` 上；`process` 的 stdin、kill、exit 与信号事件、cpuUsage 等；`timers` 的 `refresh` 与 `timers/promises`；`os` 补齐。
3. **net + tls**：`TCP`、`TLS` 句柄；`net.Socket`、`net.Server`、`net.connect`、`net.createServer`；`tls.connect`、`tls.createServer`。
4. **http + https**：`HTTPParser` 句柄；Server、IncomingMessage、ServerResponse、ClientRequest、Agent、keep-alive、chunked、upgrade。fetch 的对照用例随之并入 `tests/node`。
5. **http2**：`Http2Session` 句柄；客户端与服务端的 session 和 stream。

Node 的模块按需在首次导入时求值，不增加 realm 的启动成本；Buffer 的方法与 `global`、`atob`、`btoa` 属于全局环境，在 realm 初始化时装好。

### 不采用的方案

- 移植 Node 的 lib JS：语义最接近，但要引入数千行代码和 primordials、internalBinding 的对接层。用户选择运行时自己实现。
- 在 Rust 中实现完整的 API 对象（Bun 的做法）：只能用 JSC 的 C API 逐个手写方法，子类化、原型补丁和事件顺序都要在 Rust 里重新保证。
- 用 npm polyfill（readable-stream 等）。

## 兼容范围

用户决定（2026-10-27）：
- 运行时自己的 Node 模块用 TypeScript 写成 ES 模块，以 `@types/node` 约束签名，并按 250 行规则拆分；prelude、web、async 以后另行迁移。


- 支持 net、http、http2。
- Rust 实现资源桥接，以注入的原生句柄对象交给 JS；Node 的类是包在句柄外的薄 JS。
- pid 等用真实值，环境仍精选。
- 按上面的五个阶段推进。

**变更**
- 新增上述内置模块与全局对象。
- `process.pid` 等从虚拟值改为真实值（第二阶段）。

**不变**
- 插件接口、`ply:` 模块、CLI、存储格式；模型 bash 工具的执行路径与环境规则。

## 验收

- 每个阶段：新增的 `tests/node` 用例由 Node 24.14.0 录制、`npm run compat:check` 复核，`cargo test -p ply-cli --test node_compat` 通过，有意差异登记理由；完整 workspace 测试和 `npm run check` 通过。
- 第三至五阶段的用例在同一个脚本里起服务器（端口 0）再连接，Node 和 Ply 跑同一份脚本做对照。

## 规范同步

每个阶段完成后更新 SPEC-PLUGINS 中 Node API 的范围，在本 RFC 记录该阶段的验证结果。

## 验证记录

### 第一阶段（2026-10-27）

- 新增运行时模块：
  - `stream`：Readable、Writable、Duplex、Transform、PassThrough、legacy Stream、`pipeline`、`finished`、`addAbortSignal`、`Readable.from`、异步迭代，以及 `stream/promises`；
  - `string_decoder`；
  - `events`：完整的 EventEmitter，模块级的 `once`、`on`、`getEventListeners`、`addAbortListener`、`errorMonitor`；
  - `util`：`inherits`、`deprecate`、`callbackify`、`debuglog`、`styleText`、`stripVTControlCharacters`、`toUSVString` 等，以及 `util/types`；
  - `buffer`：读写族、`write`、`copy`、`compare`、`indexOf` 系列、`fill`、`swap`，utf16le 与 ascii 编码，`atob`、`btoa`、`isUtf8`、`isAscii`、`constants`；
  - 全局 `global`、`atob`、`btoa`。
- EventEmitter、Readable、Writable、Duplex 都是普通构造函数，`X.call(this)` 配合 `util.inherits` 的旧式子类与 `class extends` 都有用例覆盖。
- 对照用例由 26 个增加到 36 个，新增 `stream`（5 个）、`string_decoder`、`globals` 三个类别，并扩充了 buffer、events、util。期望值由 Node 24.14.0 录制，`npm run compat:check` 复核通过；Ply 首次比对即全部一致，包括流事件的顺序。
- prelude 降级后的结果改为每个进程缓存，不再在每个 realm 创建时重新降级。
- `npm run check` 通过：其中 `tsc` 用 `@types/node` 24.13.6 核对运行时模块的签名；ESLint 和 250 行规则覆盖 `crates/ply-jsc/src/node`；可见面检查拒绝插件导入 `ply:runtime`。
- 完整的 `cargo test --workspace --offline`：207 个测试中 206 个通过。失败的是一个 TUI 用例，原因是并行运行时 `openpty` 申请伪终端失败，测试逻辑并没有执行；单独重跑两次 TUI 测试均为 14/14 通过。
