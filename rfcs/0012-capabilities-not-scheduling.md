# RFC 0012: 插件只见能力，fiber 只在内核

状态：已实现。
关联规范：SPEC-PLUGINS、SPEC-TYPES、SPEC-FIBER、SPEC-AGENT、SPEC-CONTEXT、SPEC-TOOLS、SPEC-SKILLS-MEMORY。

## 问题与证据

fiber 是内核的调度单位。续体归属本来已经在 Rust 里：注册 Promise reaction 时，原生对象捕获消费方 scope（[runtime.rs](../crates/ply-jsc/src/runtime.rs) 的 `bind_continuation`），JS 拿不到。但内核又把调度信息交回了插件：

- 全局 `fiber`：`info()` 返回 id、parent、ancestors；另有 `record()`、`limitChildren()`、`childBudget()`。
- `ply:lifecycle`：`current()` 返回 plugin、loading、host、client、fiber、ancestors；`onDispose(cb)` 按调用注册清理。
- 插件据此用调用方身份重建调用树：
  - context 用 `owners: Map<fiberId, conversationId>` 沿祖先链找"当前会话"，用 `current().plugin` 决定 block 来源，用 `host` 放行任意会话（原 `plugins/context/ownership.ts`）；
  - agent 用 `activeUsage: Map<fiberId, RunUsage>` 沿祖先链找父 agent，继承策略、报告要求和用量链（[run.ts](../plugins/agent/run.ts)）；
  - jev 通过 `ctx.reportExternalModelCall` 把用量挂到最近的 agent 祖先（[usage.ts](../plugins/agent/usage.ts)）；
  - TUI 用 `ancestors.includes(session.fiber)` 判断审批是否属于自己的会话（[tui/main.ts](../plugins/tui/main.ts)）；
  - bash 和 approval 用 `fiberId + ":approval"` 拼审批 id，agent 校验 `source.fiber` 等于自己（[bash/main.ts](../plugins/bash/main.ts)、[agent/tools.ts](../plugins/agent/tools.ts)）；
  - 内核 `kernel.memory_policy` 数祖先链里有几个 `agent`，决定 memory 能否写入，插件名写死在内核里（[lib.rs](../crates/ply-kernel/src/lib.rs)）。
- 全局 `ctx` 与 `ply:plugins` 句柄是同一份 onLoad 挂载的两个入口；同名时后挂载覆盖，调用方不知道自己调到了谁。
- `plugins.x.loaded` 让插件自己处理加载顺序，共 17 处。
- `fiber.record` 只被 `--record` 和测试读取。内容与 Spawn/Exit 的输入输出或插件事件重叠，是第二种"带归属的 payload"。
- 取消时 `retire_scope` 直接丢弃该调用排队中的续体，`finally` 不执行。agent 和 tui 因此用 `onDispose` 把 `finally` 里的清理再写一遍。

结果：同一棵调用树在内核和插件 Map 里各有一份定义；插件互相比较可伪造的字符串；按调用建立的 Map 要手动清理；插件既要关心能力，又要关心调度。

## 方案

### 原则

插件只看到三样东西：自己的 input、自己调用的东西的返回值、自己持有的值（包括插件目录及其挂载）。插件 API 中不出现 fiber、调用方、祖先和加载顺序。调用树、归属、取消传播、深度上限、加载顺序和事件归属由内核负责。插件代码本来就在可信边界内，插件之间不再用调用方身份做所有权检查。

### 改后的插件可见面

入口导出的 hook：

| hook | 时机 |
|---|---|
| `onLoad(options)` | 插件加载时一次 |
| `onHandle(input)` | 每次调用，即一个 fiber |
| `onDispose()` | 新增。realm 回收时一次，与 onLoad 对称 |

全局：

| 全局 | 处理 |
|---|---|
| `console`、`process`、`Buffer`、`TextEncoder`、`TextDecoder`、`structuredClone`、定时器、`queueMicrotask` | 保留 |
| `Promise`（运行时替换，`then` 被改写） | 保留为内部机制，JS 中不携带身份 |
| `fiber` | 删除 |
| `ctx` | 删除，改用 `ply:plugins` 的具名句柄 |

模块作用域的 `require`、`exports`、`import.meta`、`import()`，以及 node 内置模块（fs、fs/promises、path、path/posix、process、buffer、events、util、os、timers、timers/promises、child_process）不变。

`ply:` 模块：

| 模块 | 处理 |
|---|---|
| `ply:plugins` | 保留调用、`description`、`parameters`、`wrapsInput` 和挂载；删除 `.loaded` |
| `ply:lifecycle` | 整个模块删除 |
| `ply:events` | 保留；`Event.source` 只剩 `plugin` |
| `ply:commands`、`ply:sh`、`ply:terminal`、`ply:http` | 本 RFC 不改 |
| `ply:models`、`ply:memory-store`、`ply:checkpoints`、`ply:skills`、`ply:markdown` | 本 RFC 不改；它们是否应作为 `ply:` 模块另行讨论 |
| `ply:async/helpers/*` | 编译器内部使用 |

SDK 声明随之删除：`packages/sdk/modules/runtime.d.ts`（`fiber`、`ctx` 和 `PlyChildBudget`）与 `packages/sdk/modules/lifecycle.d.ts` 整个删除；[approval.d.ts](../packages/sdk/modules/approval.d.ts) 与 [events.d.ts](../packages/sdk/modules/events.d.ts) 中的 fiber 字段。

### 取消时执行 finally

取消包括：宿主 `cancel_fiber`、父级取消传播、事件分发超时，以及 CPU 或内存超限。取消后分两个阶段处理。

1. **清理阶段**。该调用的 Exit 立即以 `cancelled` 或 `budget_exceeded` 发出，与现行为一致。随后：
   - 该调用未完成的宿主操作（子调用、方法调用、IO、HTTP、定时等待）停止，对应的 Promise 以取消错误 reject。错误为 `Error`，`name` 是 `AbortError`，`code` 是 `ERR_PLY_CANCELLED`。
   - 该调用对**不是它自己创建的** Promise 的等待被中断，以同一错误继续执行。
   - 它自己用 `new Promise` 等创建的叶子 Promise 以同一错误 reject；其他调用若在等待这些 Promise，同样收到这个拒绝；定时器回调不再触发。
   - 它自己的派生 Promise（async 函数的结果、`then` 的结果）**不**直接拒绝，而是沿着链条自然结束，这样外层帧的 `catch`/`finally` 不会被内层的拒绝短路。
   - CPU 或内存超限终止的那一段同步执行中，正在执行的 async 帧不会再继续，它们的结果 Promise 以同一错误 reject，等待它们的外层帧照常清理。
   - 每个在取消前已存在的 async 帧恰好收到一次 `AbortError`：要么在取消后第一次恢复时（即使它等待的值已经成功返回），要么由一次同步宿主调用当场抛出。清理阶段新开始的帧不受影响，所以 `finally` 里的 `await` 正常工作。
   - 取消请求在一段 JS 执行中途到达时，下一次宿主调用抛出 `AbortError`，清理阶段从这一刻开始，此后同一段里的 `catch`/`finally` 可以做 IO。已经完成的宿主操作仍返回它的结果。
   - 只中止、拒绝取消那一刻已经存在的操作、定时器和等待；清理阶段新开始的工作不受影响，直到回收。
   - 续体照常调度，`catch` 和 `finally` 正常执行。清理代码可以调用挂载方法、做宿主 IO（例如关闭会话、释放租约）；新的子调用（onHandle）被拒绝，错误码 `ERR_PLY_CANCELLED`。
   - 父级被取消时，子调用各自进入清理阶段。父级不等待子级清理完成，与现行为一致。
   - 清理阶段中的未处理 rejection 不改变终态。onHandle 在此阶段返回的值或抛出的错误，也不改变已经发出的 Exit。
2. **回收**。清理阶段的整段墙钟期限是 2 秒，沿用现在 onDispose 批次的期限；每次进入 JS 另有 CPU 预算。清理阶段结束或期限到达后，执行现在 `retire_scope` 的回收：丢弃剩余续体，释放终端、HTTP、宿主任务和定时器。

清理过程用 `FiberEvent::Cleanup` 记录：owner 为该 fiber，先发一次 `started`，再发一次 `done`、`failed` 或 `timed_out`。

边界：CPU 或内存超限时，被终止的那一段同步执行内部的 `finally` 不会执行，因为 JSC 的终止异常无法捕获；外层正在 await 的函数会收到 `AbortError`，它们的 `finally` 照常执行。清理不回滚已经发生的 IO 或共享状态。成功路径不变：正常结束的 onHandle 不额外回收未 await 的定时器，保持 Node 语义。

### onDispose 导出 hook

- 签名为 `export function onDispose(): void | Promise<void>`，不接收参数，也拿不到任何调用信息。
- 内核回收插件 realm 时调用一次。回收发生在两种情况：宿主调用新增的 `Kernel::close()`；onLoad 失败后回收 realm（内核没有插件替换，不存在第三种）。入口模块求值失败时没有导出可调用，跳过。
- 顺序：先取消该 realm 中仍在运行的调用，等它们的清理阶段结束（最多 2 秒）；再调用 onDispose（最多 2 秒，另有 CPU 预算）；最后关闭 realm。`Kernel::close()` 按加载完成的逆序回收各插件。
- onDispose 的异常和超时记为 `Cleanup` 的 `failed` 或 `timed_out`，不影响其他插件的回收。CLI 退出前调用 `Kernel::close()`。
- 现有的按调用清理全部迁到 `finally`：agent 和 tui 的 `onDispose(cb)` 删除，由已有的 `finally` 承担。

### 删除 record

删除 `fiber.record`、`kernel.record` op 和 `FiberEvent::Record`，`--record` 的 JSONL 中不再出现 `record` 行。

只能在运行过程中记下的信息，改为插件自己 `events.define` 的事件。`emit` 时内核记录 `FiberEvent::Plugin`，附带插件名和 fiber；没有订阅者时只记录，不分发。

| 原 record | 改为事件 |
|---|---|
| model_call_started、model_call | `agent.model_call_started`、`agent.model_call`；jev 的对应为 `jev.model_call_started`、`jev.model_call` |
| context_compaction、context_compaction_deferred | `agent.context_compaction`，用 `status` 区分 completed、failed、deferred |
| agent_policy | `agent.policy` |
| agent_checkpoint | `agent.checkpoint` |
| agent_report_rejected、agent_report_validated | `agent.report`，用 `status` 区分 |
| jev_check_started、jev_check | `jev.check_started`、`jev.check` |
| approval_bypassed、approval_result | `approval.bypassed`、`approval.result` |

以下直接删除，信息已在返回值、Spawn 输入或其他事件中：agent_usage、agent_approval_required、agent_tool_error（在 agent 结果或工具结果中）；approval_requested（与 `approval.request`、`approval.toolRequest` 事件重复）；tool_approved、command_approved、command_approval_required（在工具输出中）；memory_operation（在 memory 结果中）；tui_session、tui_approval（在 TUI 结果和监听器的 Exit 中）。示例插件改为发事件或写 console。

### 加载顺序归内核

- 内核读取签名时遍历入口模块图（入口及其相对 import），静态收集 `import { a } from "ply:plugins"` 中的具名导入，作为该插件 onLoad 的依赖。
- 插件的 onLoad 在依赖全部结束 onLoad 后才开始。依赖加载失败时，本插件加载失败，错误中指明是哪个依赖；依赖成环视为加载错误。
- 通过默认导入 `plugins[x]` 的动态访问不建立依赖。对 onHandle 和挂载方法的调用仍由内核等待目标插件 onLoad 结束，与现行为一致。
- `events.on("<plugin>.<name>")` 由内核等待事件所属插件的 onLoad 结束后再订阅。
- 同步读取挂载的数据（例如 `context.Type`），只有在该插件 onLoad 成功之后才有值；在 onLoad 里，具名导入的加载顺序保证了这一点。

### 内核与运行时的其他改动

- 删除以下 op：`kernel.fiber_info`、`kernel.record`、`kernel.limit_children`、`kernel.child_budget`、`kernel.memory_policy`、`invocation.info`、`fiber.watch`、`fiber.unwatch`。`fiber.unhandled` 改名，prelude 和 async.js 中不再出现 fiber 一词。
- `ActiveFiber.child_budgets` 和 `KernelError::ChildLimit` 删除；通用深度上限保留。
- `Kernel::method` 不再构造 origin（祖先链等）。挂载方法在被调插件的 realm 中运行，续体仍按调用方的 fiber 归属。
- 事件信封改为 `source: { plugin }`；`FiberEvent::Plugin` 继续记录 fiber。
- 删除 `Kernel::call_ctx`，宿主改用 `call_plugin_method`。新增 `Kernel::close()`。

### 插件迁移

**context**
- 删除 `ownership.ts` 中的 owners 映射、祖先查找、host 特权和"属于另一个会话"检查（整个文件删除，按 id 取会话的函数移入 `state.ts`）。会话 id 就是能力，谁持有谁就能操作。
- `openTask`、`restoreTask` 和 `restoreConversation` 增加可选参数 `inherit`（父会话 id，`null` 视同不给）。给出时，从父会话复制只读快照（去掉 `source` 为 context 的 block），且不刷新 provider；不给出时从模板开始。
- `set` 必须显式给出 `source`，即调用方插件自己的名字；provider 也按 `source` 回调。不给 `conversation` 时改的是模板，给出时改的是该会话；不再根据调用方推断。
- `blocks()` 不传 id 时返回模板；`closeConversation` 的 id 改为必填。
- 删除 `refresh` 方法。context 在追加完整的工具批次后刷新 Reference provider；内容相同时不产生新版本，沿用现规则。
- 打开会话时不再等待来源插件加载完成。

**agent**
- 父子关系改用委派凭据。父 agent 的工具执行器为每次 agent 工具调用生成一个不可猜测的一次性凭据，登记到 `delegations: Map<凭据, RunUsage>`，作为 `delegation` 字段放入子调用的输入（总是覆盖模型提供的值），调用结束后注销。
- 子 agent 用凭据取得父 run，从而继承策略上限、报告要求、用量链，并把父会话 id 作为 context 的 `inherit` 传入。输入中没有凭据即为根调用；凭据未知时拒绝执行。
- 该字段会出现在 agent 工具的 schema 中，JSDoc 注明它由父 agent 填写。
- 子 agent 配额：父 run 保存 `{ limit, started }`。在审批完成之后、调用 `plugins.agent` 之前同步检查并计数；同一 realm 是单线程，检查与计数之间没有 await，因此是原子的。超限时返回工具错误，不会产生 Spawn。失败、完成、替换和恢复都不退还配额；恢复时从快照的 `children_started` 恢复，格式 4 不变。
- 删除按 fiber 索引的 `activeUsage`；run 对象经凭据链接到父 run，模型调用照旧向自身和各祖先 run 累加。
- 删除 `reportExternalModelCall`。jev 等旁路插件的用量只记录在它们自己的事件中，不计入 agent 的 `total_usage`。
- `onDispose` 回调、`fiber.*` 和 `await context.loaded` 全部删除。

**approval、bash**
- 审批请求的 id 由 approval 在自己的 realm 内单调生成，只用于展示和日志；删除 `source` 字段。
- bash 不再生成 id，改为校验返回的 request 中 command 和 cwd 与提交时一致；agent 校验 tool、cwd、input 一致。不一致时视为无效审批。
- `ctx.*` 改为 `import { approval } from "ply:plugins"`；`await plugins.bash.loaded` 删除。

**tui**
- 删除 `Session.fiber` 和祖先判断：有活动会话时由该会话处理审批，否则返回 pending。同一进程目前只有一个 TUI 会话；将来多个会话并发时如何按调用树路由审批，由内核负责，另立 RFC。
- `ctx.approvalStatus` 改为 `approval.approvalStatus`；`.loaded` 改为具名导入，由内核排序。

**memory、jev 及其他**
- memory 删除对 `context.refresh` 的调用和 `context_warning`。写权限只由 agent 的授权决定：`policyFor` 已经不给子级 memory 工具。
- jev 删除 `reportExternalModelCall`，改发 jev 事件。
- skill、create、edit、read、remove、deepseek、mimo 删除 `await context.loaded`。

### 不采用的方案

- 只把 fiber 改名为 invocation：身份仍然留在插件里。
- 调用局部变量（`local()` 或 AsyncLocalStorage）：虽然不暴露 id，插件仍然要推理调用树。
- 每次调用失败后调用导出的 onDispose：导出函数拿不到那次调用闭包里的状态；要清理，就得知道是哪次调用。
- 把 context 贡献改为 onLoad 的返回值，以恢复"失败的 onLoad 不发布贡献"：这会改变 context 的贡献接口，留待后续单独讨论。

## 兼容范围

用户决定（2026-10-27）：插件关心能力而不是调度，插件中不出现调用方信息；`ply:lifecycle` 不应存在；onDispose 作为与 onLoad 对称的导出 hook；删除 record；按清单删除 `fiber`、`ctx` 和 `.loaded`。不提供旧接口的兼容层。

**破坏性变更**
- SDK：删除 `fiber`、`ctx`、`ply:lifecycle` 和 `.loaded`；approval 与 events 类型删除 fiber 字段。
- context 方法签名：新增 `inherit`；`set` 必须给 `source`；删除 `refresh`；`closeConversation` 的 id 必填。
- agent 输入新增 `delegation` 字段。
- Rust API：删除 `Kernel::call_ctx`、`FiberEvent::Record` 和 `KernelError::ChildLimit`；新增 `Kernel::close()`。
- `--record` 的 JSONL 中不再有 `record` 行，对应信息改为 `plugin` 行。

**行为变更**
- 取消后 `finally` 会执行，以前不会。
- onLoad 失败后，它此前调用其他插件方法造成的状态不再回滚，context 也不再等待来源插件的加载结果。宿主必须在 `ready()` 成功之后才开始服务，CLI 已经如此。
- agent 的 `total_usage` 不再包含 jev 等旁路插件的模型用量；完整的子树用量由读取事件流的一方按 Spawn 父子关系汇总。
- TUI 不再区分审批来自哪棵子树。
- memory 写权限只由 agent 授权控制，内核不再按祖先检查。
- 子 agent 配额由 agent 插件执行，不再由内核准入。

**不变**
- CLI 的命令与参数。
- onHandle、onLoad 和 `ply:plugins` 的调用语义。
- Rust 层的 Spawn、Exit、Cleanup、Plugin 事件，以及四种终态和深度上限。
- agent 恢复格式 4、context 恢复格式 2、memory 存储格式。

## 验收

**自动检查**
- `npm ci --ignore-scripts` 和 `npm run check`。check 新增插件可见面检查：`plugins/`、`examples/plugins/`、`packages/sdk/`、prelude.js 和 async.js 中不得出现 fiber、`ply:lifecycle` 或全局 `ctx`。
- `cargo test --workspace --locked --offline`，包括 CLI、MCP、真实 PTY 和目录插件。

**针对性回归**
1. 取消：父级 await 子级期间，宿主取消父级；父子两级的 `finally` 都执行，并完成 IO 清理。被 await 的 Promise 的错误 `name` 为 `AbortError`、`code` 为 `ERR_PLY_CANCELLED`。每个调用只有一次 Exit，状态为 `cancelled`。
2. 清理期限：`finally` 中无限等待；2 秒后发出 Cleanup `timed_out`，剩余续体被丢弃，定时器、HTTP 和终端被释放。
3. CPU 超限：被终止那一段内部的 `finally` 不执行，外层 await 帧的 `finally` 执行；Exit 为 `budget_exceeded`。
4. 清理阶段发起新的子调用会被拒绝；挂载方法调用和宿主 IO 允许。
5. 共享 Promise：被取消调用产生的 Promise 对其他调用以 `AbortError` 拒绝，其他调用不受其他影响。
6. onDispose：`Kernel::close()` 时每个插件恰好调用一次，顺序为加载逆序，且在在途调用清理之后；onLoad 失败后回收 realm 时也调用；异常和超时记为 Cleanup。
7. 运行时可见面：`typeof fiber` 和 `typeof ctx` 都是 `"undefined"`；导入 `ply:lifecycle` 报模块不存在；`plugins.x.loaded` 为 undefined。
8. 加载顺序：具名导入的依赖先完成 onLoad；依赖失败时依赖方失败并指明依赖；成环时报错；默认导入不参与排序；`events.on` 等待事件所属插件加载完成。
9. 子 agent 配额：同一轮用 `Promise.all` 发起三个 agent 工具调用、上限为 2 时，恰好产生 2 个 Spawn，第三个返回工具错误；审批被拒绝时不消耗配额；恢复后计数延续。
10. 委派凭据：模型提供的 `delegation` 被覆盖；未知凭据被拒绝；子 agent 继承策略上限、报告要求和父会话快照，且不能更新父会话。
11. context：会话 id 显式传入；不给 conversation 的 `set` 只改模板；工具批次之后刷新 Reference；内容相同时不产生新版本。
12. 审批：bash 和模型工具的审批请求都不含 source；返回内容与提交不一致时判为无效。TUI 有活动会话时处理审批，没有时返回 pending。
13. 事件记录：`agent.model_call_started` 与 `agent.model_call` 在 JSONL 的 `plugin` 行中成对出现，并带 fiber 归属；JSONL 中没有 `record` 行。
14. memory：子 agent 默认没有 memory 工具；根 agent 的写入行为不变。

本 RFC 不改变模型连接，不需要真实供应商调用。

## 规范同步

实现后同步以下内容：

- **SPEC-PLUGINS**：三个导出 hook；加载顺序规则；删除"`ctx`、`fiber` 仍是运行时全局"。
- **SPEC-TYPES**：SDK 声明的变化。
- **SPEC-FIBER**：插件不可见原则、取消时的清理阶段与期限、onDispose 语义。
- **SPEC-AGENT**：配额由 agent 执行；委派凭据；用量汇总的范围；事件取代记录。
- **SPEC-CONTEXT**：显式会话 id、`inherit`、刷新时机。
- **SPEC-TOOLS**：审批请求字段、TUI 规则、事件信封。
- **SPEC-SKILLS-MEMORY**：写权限的来源；memory 不再推送刷新。
- **验收矩阵**：更新"fiber 调度与归属""清理生命周期""用量与事件"三行。
- **其他文档**：README 中关于 fiber 全局与 `ply:lifecycle` 的说明；在 RFC 0001 的"运行中的 fiber"一节注明已被本 RFC 取代。

## 验证结果（2026-10-27）

- `npm ci --offline --ignore-scripts`、`npm run check` 通过：strict 类型、ESLint、文档链接，以及新增的插件可见面检查（插件、示例、SDK、prelude 与 async.js 中没有 fiber、`ply:lifecycle`、全局 `ctx` 或 `.loaded`）。
- `cargo test --workspace --locked --offline` 共 198 项通过、0 失败，覆盖 CLI、MCP、真实 PTY 与目录插件。
- 新增回归：JSC 运行时的取消与清理（`node_outlet.rs`）、父子调用的清理与清理期间拒绝新调用（`cleanup.rs`）、加载顺序与 onDispose（`loading.rs`），以及 context、agent、approval、jev、TUI、CLI 中第 9–14 项。原先依赖时序的两个取消用例重复运行 8–12 次无失败。
- 实现中对方案的两处细化已写入上文：每个 async 帧只收到一次 `AbortError`；中止只针对取消那一刻已存在的工作。
- 本 RFC 不改变模型连接，未发起真实供应商调用。
