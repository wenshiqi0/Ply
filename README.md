# Ply

Ply 是一个 agent harness：Rust 内核负责调度、IO 和记录，插件用 TypeScript 写，运行在内嵌的 JavaScriptCore 里。一个目录就是一个插件，导出的函数签名就是它的工具定义；内置的 agent、上下文、审批和终端界面也都是普通插件。

目前只支持 macOS（使用系统 JavaScriptCore）。

## 特点

- **插件只写能力**：一个插件最多导出 `onHandle`、`onLoad`、`onDispose` 三个钩子。调度、调用归属、取消、清理和加载顺序都在 Rust 内核里，插件代码看不到 fiber，也不关心是谁调用了它（[RFC 0012](rfcs/0012-capabilities-not-scheduling.md)）。
- **调用就是 `await`**：`await plugins.bash({ command })` 的那一刻就是一次调用，内核把它作为子调用调度；并发直接用 `Promise.all`。
- **挂载式 IO**：文件 API 通过挂载访问存储，后端可在磁盘、内存、SQLite 之间切换；模型的 `bash` 工具由内置的 Ply sh 执行。挂载不等于 OS 沙箱。
- **自带 agent**：分层上下文与自动压缩、子 agent 委派与配额、逐次模型调用的用量统计、失败后从快照恢复。模型层是内嵌的 [pi-ai](https://github.com/earendil-works/pi/tree/main/packages/ai)，当前接入 DeepSeek 与 MiMo。
- **审批与安全**：环境审批协议，真实终端的 TUI 审批，可选的 Jev 命令风险审查。
- **多种入口**：CLI、脚本、MCP server、TUI；`--record` 把每个调用的开始、事件、结束与清理写成 JSON lines。

## 快速开始

需要 macOS 和 Rust 工具链。

```bash
cargo build -p ply-cli
```

默认模型是 DeepSeek，在宿主环境配置 `DEEPSEEK_API_KEY` 后：

```bash
./target/debug/ply tui
```

```bash
./target/debug/ply call --record fibers.jsonl agent '{"prompt": "把 app.py 的输出改成 hello, ply，然后运行验证"}'
```

作为 MCP server 给其他 agent 使用（例如 Claude Code）：

```bash
claude mcp add ply -- /path/to/ply/target/debug/ply mcp --record /tmp/ply-fibers.jsonl
```

不调用模型，直接使用工具：

```bash
./target/debug/ply tools
```

```bash
./target/debug/ply call read '{"path": "README.md", "offset": 1, "limit": 80}'
```

## 写一个插件

目录名就是插件名，`main.ts` 用具名导出提供钩子：

```ts
// todos/main.ts
import plugins, { context } from "ply:plugins";

/** Count TODO lines in a file. */
export async function onHandle(input: { path: string }): Promise<{ todos: number; lines: number }> {
  const file = JSON.stringify(input.path);
  const [grep, wc] = await Promise.all([
    plugins.bash({ command: `grep -c TODO ${file}` }),   // 调用另一个插件：内核把它作为子调用调度
    plugins.bash({ command: `wc -l < ${file}` }),
  ]);
  return { todos: Number(grep.stdout.trim()), lines: Number(wc.stdout.trim()) };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  // 具名导入 context：内核保证它的 onLoad 先完成
  await context.registerTool("todos");
}
```

```bash
./target/debug/ply call --plugin ./todos todos '{"path": "README.md"}'
```

- **`onHandle(input)`**：被 await 时运行一次。它的 JSDoc 是插件描述，参数类型和返回类型就是工具的输入和输出，加载时从签名里读出。参数校验由插件自己负责。
- **`onLoad(options)`**：加载时在后台运行一次，接收宿主为该插件配置的 JSON（未指定时为 `{}`）。返回的数据和函数通过该插件的句柄访问：数据是一份拷贝，函数在所属插件里执行。
  - 用 `import { x } from "ply:plugins"` 具名导入的插件，内核保证它的 `onLoad` 先完成；它加载失败时本插件也加载失败，相互导入成环会报错。默认导入 `plugins.x` 不建立顺序，调用照常等待目标加载完成。
- **`onDispose()`**：内核回收插件 realm 时运行一次（宿主关闭内核，或 `onLoad` 失败之后），两秒期限。每次调用自己的清理写在 `finally` 里：调用被取消时，它在等待的东西以 `AbortError`（`code: "ERR_PLY_CANCELLED"`）拒绝，`catch`/`finally` 照常运行。
- **共享闭包**：同一个插件的多次调用共享模块里的状态，在 `await` 处交替执行。
- **列举插件**：`Object.keys(plugins)` 列出已加载的插件；有 `onHandle` 的插件还带 `plugins.x.parameters`，即它作为工具时的参数 schema。
- **Node 模块照常用**（`fs`、`path`、`child_process` 等），由 JSC shim 提供；Node 类型只用于编辑器，实际支持范围以运行时及其测试为准。Ply 模块与插件句柄的类型声明入口是 [packages/sdk/index.d.ts](packages/sdk/index.d.ts)。
- **注册命令**：插件可以在 `onLoad` 里通过 `ply:commands` 注册顶层 CLI 命令，见 [命令与模块接口](rfcs/0004-commands.md)。

插件向模型上下文提供内容统一走 context：`context.set({ source: "skill", type: context.Type.Extensions, key: "skills", content })`。`source` 是插件自己的名字；不指定数字 order，也不注册构建回调，类型决定顺序和更新策略，详见 [Context](rfcs/0007-context.md)。

更多示例在 [examples/plugins](examples/plugins/)：bash 执行前的 hook、审批环境适配器模板、注册顶层命令。

## 内置插件

默认加载 DeepSeek 接入；用 `--model mimo-v2.6-pro` 或 `--model mimo-v2.6-flash` 切换为 MiMo，两者只加载一个。

| 插件 | 钩子 | 做什么 |
|---|---|---|
| [agent](plugins/agent/main.ts) | onHandle、onLoad | 用语言模型完成一个任务，并把自己注册到 context，允许模型开启子 agent |
| [context](plugins/context/main.ts) | onLoad | 按 Rules/Project/Extensions/State/Reference 管理上下文，冻结前缀并追加状态更新，负责工具快照、预算、压缩和恢复 |
| [deepseek](plugins/deepseek/main.ts) | onLoad | DeepSeek 接入：告诉 context 使用 `deepseek-v4-flash`，密钥在宿主的 `DEEPSEEK_API_KEY` |
| [mimo](plugins/mimo/main.ts) | onLoad | 可选的 MiMo V2.6 接入，密钥在宿主的 `MIMO_API_KEY`，支持 Token Plan 和按量付费 API |
| [bash](plugins/bash/main.ts) | onHandle、onLoad | 用内置 Ply sh 执行脚本；执行前等待 `bash.beforeExecute` 事件 |
| [read](plugins/read/main.ts) | onHandle、onLoad | 从挂载存储读取 UTF-8 文本，支持按行分页 |
| [create](plugins/create/main.ts) | onHandle、onLoad | 新建文件 |
| [edit](plugins/edit/main.ts) | onHandle、onLoad | 精确替换文件里的一段文字 |
| [remove](plugins/remove/main.ts) | onHandle、onLoad | 删除文件或目录 |
| [memory](plugins/memory/main.ts) | onHandle、onLoad | 显式记住、跨会话召回和遗忘；私有文件存储，由 context 注入，见 [Memory](rfcs/0008-memory.md) |
| [skill](plugins/skill/main.ts) | onLoad | 发现 SKILL.md 元数据并注入上下文，模型通过 read 加载完整指令，见 [Skills](rfcs/0006-skills.md) |
| [approval](plugins/approval/main.ts) | onLoad | 可选的审批策略与协调；通过 `approval.request` / `approval.toolRequest` 事件对接环境适配器，自己不批准 |
| [tui](plugins/tui/main.ts) | onHandle、onLoad | 可选的终端交互与审批界面；只调用 agent，工具集和委派由 agent 管理 |
| [jev](plugins/jev/main.ts) | onLoad | 可选的 TypeSafe Jev 安全检查，订阅 `bash.beforeExecute`；用 `--jev` 启用，密钥在宿主的 `TYPESAFE_API_KEY` |

context、deepseek、mimo 等没有 `onHandle` 的插件不能被调用，也不会作为工具透出给模型或 MCP 客户端。

## 命令行

| 命令 | 做什么 |
|---|---|
| `ply tools` | 列出已加载插件及其工具 schema |
| `ply call PLUGIN JSON` | 调用一个插件并输出结果 |
| `ply run FILE.ts -- ARGS` | 像 node 一样运行一个 TS 文件，定时器跑完才退出；通过 `ply:plugins` 导入插件目录 |
| `ply mcp` | 以 stdio JSON-RPC 提供 MCP 工具服务 |
| `ply tui` | 交互任务与终端审批 |
| `ply commands` | 列出插件注册的顶层命令，例如 `ply memory` |

常用选项：

- `--mount SPEC`：`AT=disk:PATH[:ro]`、`AT=sqlite:PATH[:ro]`、`AT=memory`，或 `disk:PATH[:ro]`（挂在它自己的绝对路径上），可以重复。默认把当前目录以读写方式挂在它自己的路径上。
- `--plugin DIR`：加载一个插件目录，可以重复；`--no-default-plugins` 不加载内置插件。
- `--plugin-options NAME=JSON`：把 JSON 传给对应插件的 `onLoad`，由插件解释。
- `--model MODEL`：选择 `deepseek-*` 或 `mimo-*` 模型；`--mimo-provider PROVIDER` 选择 MiMo 的服务或地区。
- `--jev`：启用 Jev bash 安全审查。
- `--record FILE`：把每个调用事件（spawn / exit / cleanup / plugin）以 JSON lines 追加到文件。

agent、上下文和审批相关的其他选项见下面的参考部分。

## 架构

| crate | 内容 |
|---|---|
| `ply-core` | 描述层：插件、输入输出类型、`onLoad` 的挂载、fiber、fiber 事件和 hook |
| `ply-kernel` | 加载插件（读出签名与具名导入，按依赖后台运行 `onLoad`）、调度 fiber、命令注册表、发出事件、`ready` / `close`、内置插件 |
| `ply-jsc` | 内嵌 JavaScriptCore，直接运行 TS；每个插件一个 realm、一个线程；Node 风格 API、ply 模块，以及调用归属与取消清理的续体调度 |
| `ply-io` | 挂载表（mount / unmount / swap，只有挂载的路径存在）；在磁盘挂载上启动真实进程 |
| `ply-storage` | 磁盘 / 内存 / SQLite 后端，同一套行为测试同时覆盖三种 |
| `ply-sh` | 内部 shell 解释器及工具集，执行模型 bash 工具的脚本，见 [ply-sh](crates/ply-sh/README.md) |
| `ply-libc` | errno / path / fs / glob / time |
| `ply-cli` | tools / call / run / mcp / tui / commands，以及插件注册的顶层命令 |

每次 `onHandle`、事件回调或命令的运行是内核调度的一个 fiber。同一插件的 fiber 共享一个 JSC realm 和线程，在 `await` 处交替；不同插件在不同线程上并行。调用归属由消费方的异步续体携带，共享 Promise 后也保持独立的父子关系。异常、取消和 CPU 超限按所属调用隔离，父调用取消会传播到它等待的子调用。记录里的结束状态区分 `done`、`failed`、`cancelled` 和 `budget_exceeded`（CPU / 内存）；内存阈值是全进程水位。

设计与契约：当前有效的系统契约在 [SPEC.md](SPEC.md)，每项决策的方案和理由在 [rfcs/](rfcs/README.md)。运行时设计见 [RFC 0001](rfcs/0001-runtime.md)，插件事件与 bash hook 见 [RFC 0002](rfcs/0002-events.md)，环境审批协议见 [RFC 0003](rfcs/0003-approval.md)。

## 参考

### bash 工具与 Ply sh

模型可见的 `bash` 工具统一使用 **Ply sh**，入参为 `command` / `cwd` / `timeout_ms`。每次调用新建 shell，脚本内的状态和 cwd 独立。支持内部解释器的语法、builtin、自带工具，以及 PATH 中的宿主可执行文件；不存在的命令返回 127，无法执行返回 126。默认保留启动进程 PATH 的目录顺序，再补齐系统、Homebrew 和常见用户 bin 目录，不加载 bash/zsh 配置、alias、函数或全量环境变量。

内部工具和重定向使用挂载 IO；宿主程序需要磁盘 cwd，其自身的文件访问使用真实宿主文件系统，不受虚拟挂载权限隔离，参数中的虚拟绝对路径也不会自动重写。超时或调用取消会回收宿主进程组。Ply sh 没有 shell 进程信号，`signal` 为 null，超时通过 `timed_out` 表示。支持范围及缓冲管道限制见 [ply-sh](crates/ply-sh/README.md)。

`bash.beforeExecute` 覆盖整个工具调用：所有监听器异步完成且未拒绝后才进入 sh，此时重定向和命令替换在内的脚本内容都尚未执行。普通 hook 返回 `false` 或 `{ allow: false, reason }` 是硬拒绝；带 `requires_approval: true` 的风险结果进入环境审批，批准后继续，缺少适配器时返回 `approval_required`。`timeout_ms` 包含检查与执行时间，审批等待单独计时。

### 读取文件

读取文件优先用 `read`：`path` 必填，`offset` 是从 1 开始的行号（默认 1），`limit` 是最多返回的行数（默认 200）。返回原始文本 `content`、本页起始行 `offset`、返回行数 `lines`、文件总行数 `total_lines` 和 `has_more`；下一页从 `offset + lines` 开始。空文件返回空文本和 0 行。分页只限制返回内容，底层仍一次读取整个文件。

### 模型接入

agent 在自己的 JSC realm 内运行内嵌的 pi-ai 模型层，运行时不需要 Node、npm 或全局安装的 Pi。模型请求通过 Rust 的 HTTP/TLS 客户端发送，SSE 响应按需逐块读取并交给 pi-ai 解析；每次调用有独立的取消信号和 10 分钟总超时，请求体和累计响应体各限 16 MiB。密钥在请求时从宿主读取，不加入插件的 `process.env`、上下文或用量记录。

MiMo 默认使用中国区 Token Plan（`xiaomi-token-plan-cn`，`https://token-plan-cn.xiaomimimo.com/v1`），密钥只从宿主的 `MIMO_API_KEY` 读取：

```bash
./target/debug/ply call --model mimo-v2.6-pro --record mimo-fibers.jsonl agent '{"prompt": "只读总结这个项目，不修改文件"}'
./target/debug/ply call --model mimo-v2.6-flash agent '{"prompt": "只读检查 README 中的命令是否与实现一致"}'
```

`--mimo-provider` 可选 `xiaomi`（按量付费）、`xiaomi-token-plan-cn`、`xiaomi-token-plan-sgp`、`xiaomi-token-plan-ams`。每种服务都从 `MIMO_API_KEY` 取密钥，但按量密钥与 Token Plan 密钥不能互换。`mimo-v2.6-pro-ultraspeed` 仅在 `--mimo-provider xiaomi` 下可用。

```bash
./target/debug/ply call --model mimo-v2.6-pro --mimo-provider xiaomi agent '{"prompt": "只读总结这个项目"}'
```

MiMo 默认开启思考，历史思考内容随工具调用一起透传。Token Plan 的 SDK cost 元数据是占位值，不能解读为免费或套餐扣减金额，Ply 不把它输出为费用。型号与协议依据 [官方型号说明](https://mimo.mi.com/docs/en-US/quick-start/model)、[API 文档](https://mimo.mi.com/docs/en-US/api/chat) 和 [套餐支持说明](https://mimo.mi.com/docs/en-US/quick-start/faq/token-plan/Plans%26Pricing)（核对于 2026-10-24）；V2.5 模型 ID 已退役。`--no-default-plugins` 下可用 `--plugin ./plugins/mimo` 手动加载，不要同时加载 deepseek 和 mimo。

内嵌的 pi-ai bundle 随源码保存，普通 `cargo build` 不运行 Node。只有更新 SDK 或 bundle 时才需要构建期 Node：

```bash
cd packages/pi-ai
npm ci --ignore-scripts --registry=https://registry.npmjs.org
npm run build
```

pi-ai 固定为 `0.82.1`，依赖锁在 `packages/pi-ai/package-lock.json`，生成文件及第三方许可声明在 `crates/ply-jsc/src/vendor/`。OAuth、Bedrock 和其他 provider 尚未接入。运行时只提供资源能力、外部代码改走 npm 解析的调整见 [RFC 0013](rfcs/0013-resources-and-npm.md)（提议中）。

### agent 委派

`agent` 本身也是模型可见的工具：模型调用 `agent({ prompt, allowed_tools?, max_delegation_depth?, max_children?, report_sections?, max_turns? })` 时运行一个子 agent，父 agent 等它完成，结果作为工具输出返回；同一轮发起的多个子 agent 并发运行。每次工具调用都是父 agent 的子调用，记录里能看到完整过程。

每个子 agent 新建独立对话，只接收传入的 `prompt` 和父会话的只读上下文快照，父 agent 要在 prompt 里提供任务背景和约束。父子 agent 共用挂载存储、工作目录和模型。每个会话有独立的工具白名单，子 agent 只能收窄父级授权，默认也没有 memory 工具。

- `max_delegation_depth`：当前 agent 还能创建多少代子任务，主 agent 默认 1，子任务继承后减 1；为 0 时移除 `agent` 工具并拒绝继续委派。更深递归须由主调用显式授予，还受内核嵌套深度限制（默认 16，工具调用也计入）。
- `max_children`：每个 agent 累计创建直属子 agent 的硬上限，主调用默认 **2**；不是并发数，也不限制普通工具。agent 插件在审批之后、发起子调用之前同步检查并计数，超额请求成为工具错误，不创建子调用。失败、完成、取消以及替换和恢复都不退还名额。返回值的 `delegation` 给出 `limit`、`started`、`remaining`。CLI 的 `--agent-max-children COUNT` 设置主调用默认值，调用 JSON 中的 `max_children` 优先。
- `allowed_tools`：省略时取已注册工具的快照；显式请求父级没有的工具或更深委派会报错。用提示词限制工作范围时，仍应同时设置这些结构化权限。
- `max_turns`：默认不限制模型轮数；只有显式传入正整数时才限制对应 agent 的轮数。

只读、仅允许一层子任务的例子（工具白名单仍需配合可信插件使用，不等于 OS 沙箱）：

```bash
./target/debug/ply call agent '{"prompt":"启动两个子 agent 只读评审项目，再汇总", "allowed_tools":["read","agent"], "max_delegation_depth":1, "max_children":2, "report_sections":["架构判断","发现","验证边界"]}'
```

**最终报告**必须正常停止、非空、代码围栏闭合，正文不得残留工具控制标记；内部使用结束标记 `[[PLY_REPORT_COMPLETE]]`，返回前删除。`report_sections` 可要求指定的 Markdown 标题及非空内容，标题要求会传给子 agent，子级和恢复调用只能增加要求；CLI 可重复传 `--agent-report-section HEADING`。校验失败时发出 `agent.report`（`status: "rejected"`）并在原 agent 内重新生成完整报告，不创建替代 agent；通过后发出 `status: "validated"` 并返回 `report_validation`。这只校验格式和完成状态，不证明结论正确。带半截工具调用的 `length` 响应立即失败并保留可恢复状态。

### 用量统计

agent 的返回结果带两份用量汇总：`usage` 只统计当前 agent 自己的模型调用，`total_usage` 还包括它委派的子 agent。子 agent 的用量逐轮计入祖先，已报告的失败调用也会计入；父 agent 不会重复加上子 agent 返回的汇总。Jev 等旁路插件的模型用量只在它们自己的事件里，不计入 `total_usage`。

| usage 字段 | 含义 |
|---|---|
| `calls` / `reported_calls` / `missing_calls` | 模型调用次数 / 有有效 usage 的次数 / 缺失 usage 的次数 |
| `input_tokens` | 全部输入 token，包含未命中、缓存读取和缓存写入 |
| `uncached_input_tokens` | 普通输入 token，不含缓存读取/写入，对应 pi-ai 的 `usage.input`；未报告分项时为 null |
| `output_tokens` / `reasoning_tokens` | 输出 token / 其中的推理 token；推理量未报告时为 null |
| `cache_read_tokens` / `cache_write_tokens` | 缓存读取 / 缓存写入 token；供应商未报告分项时为 null |
| `total_tokens` | 输入 + 输出，推理 token 不重复相加 |
| `cache_hit_rate` | 累计缓存读取 token / 累计全部输入 token，按 token 加权；缓存分项未知时为 null |
| `complete` | 是否每次模型调用都有有效 usage；为 false 时是已报告部分的小计 |

没有 usage 或 SDK 的全零占位 usage 都记为缺失，不当作实测 0 用量；单次调用内部的 SDK 重试不另算一轮。

每次付费模型调用前发出 `agent.model_call_started` 事件，拿到响应或异常后发出对应的 `agent.model_call`，用 `call_index` 关联；强制取消可能只留下 started，表示用量未知而不是 0。加上 `--record fibers.jsonl` 后，插件事件写成 `event: "plugin"` 行，内核附上 `name`、`plugin`、`fiber` 和 `at_ms`。`agent.model_call` 的 `purpose` 区分 `task`、`compaction` 与 Jev 的 `safety`，并记录轮次、模型、响应 ID、停止原因、耗时、usage 或错误。统计整棵调用树时，按 spawn 的父子关系汇总 `agent.model_call` 与 `jev.model_call`，或读取根 agent 的 `total_usage`（不含 Jev），不要再叠加各级 `total_usage`。

### 上下文预算与压缩

默认总窗口为 **131072 token**，其中预留 **16384 token** 给输出，可用输入预算为 **114688 token**。配置的窗口还受模型的真实窗口约束；输出预留同时是每次任务回复的 `maxTokens`。窗口约束单次模型请求，不限制累计 token 或轮数；每个主、子 agent 会话独立应用同一配置。

```bash
./target/debug/ply call --context-config examples/context.json --record fibers.jsonl agent '{"prompt":"完整评审这个项目"}'

./target/debug/ply call --max-context-tokens 262144 --context-reserve-tokens 16384 --context-compact-trigger 0.8 --context-compact-target 0.5 agent '{"prompt":"继续长任务"}'
```

[examples/context.json](examples/context.json) 的字段：

| 配置 | 默认值 | 含义 |
|---|---:|---|
| `max_context_tokens` | 131072 | 输入与预留输出合计的窗口 |
| `reserve_output_tokens` | 16384 | 输出预留与任务请求的输出上限 |
| `compaction.enabled` | true | 是否自动摘要压缩；关闭后仍执行输入预算检查 |
| `compaction.trigger_ratio` | 0.8 | 输入估算达到可用输入预算的这一比例时触发 |
| `compaction.target_ratio` | 0.5 | 压缩希望达到的输入预算比例，必须小于触发比例 |
| `compaction.keep_recent_turns` | 4 | 原样保留的最近完整交互数，一个工具批次及其全部结果算一组 |
| `compaction.summary_max_tokens` | 2048 | 摘要请求的输出上限，不得大于输出预留 |
| `compaction.max_retries` | 1 | 每个摘要失败后的额外重试次数，范围 0–10 |
| `compaction.retry_delay_ms` | 500 | 重试间隔，范围 0–60000 ms |
| `compaction.failure_cooldown_turns` | 2 | 预算允许延后时，暂停软压缩的轮数，范围 0–100 |

显式 CLI 参数覆盖 JSON 文件，未配置项使用默认值；`--context-compaction off` 关闭自动压缩。未知字段、非正整数、无效比例和矛盾预算会报错；不会自动读取当前目录中的配置文件。

每次模型调用前检查预算：第一次按 UTF-8 字节数 / 2 加消息与工具定义开销保守估算；收到有效 usage 后，用该次实际输入 + 输出作为历史前缀的锚点，只估算之后新增的内容。**这是保护性估算，不是供应商 tokenizer 的精确计数。**

达到阈值后，用当前模型把较早的完整交互合并成摘要，保留原始任务、已有摘要中的有效事实以及最近的交互；摘要请求不提供工具。摘要生成、内容校验和上下文版本核对都成功后才替换旧历史；空摘要、截断摘要、工具调用和失败响应都保留旧历史并有限重试。重试耗尽但仍在硬预算内时延后软压缩并继续任务；超过输入硬预算则明确报错，不发送超预算的请求。压缩事件是 `agent.context_compaction`，用 `status` 区分 `completed`、`failed` 和 `deferred`。压缩改变历史前缀，可能降低之后的缓存复用率。

### 失败恢复

无法继续时，agent 在宿主私有目录 `$HOME/.ply/checkpoints/` 保存原任务、已完成交互、摘要、模型配置、权限、已消耗的直属子 agent 名额、报告校验状态和下一轮位置；错误信息和 `agent.checkpoint` 事件给出 `resume_id`。`PLY_CHECKPOINT_DIR` 可指定其他绝对路径；目录权限 0700、文件 0600，写入采用临时文件 + fsync + 原子替换。快照可能含任务和文件内容，不包含宿主 API key。

```bash
./target/debug/ply call agent '{"resume_id":"错误信息中的恢复 ID"}'
```

agent 恢复格式为 4，其中的上下文格式为 2；旧格式明确拒绝恢复，需要发起新任务。恢复时须使用相同的工作目录，会重算窗口预算、重新检查工具权限，不能扩大原授权；已花费的子 agent 名额和报告校验状态保留。恢复记录有跨进程互斥锁，成功后删除快照。执行工具前标记 `tools_pending`，工具结果全部保存后才回到 `ready`，工具执行到一半的状态不会自动重放。历史累计用量以 `previous_usage` / `previous_total_usage` 单列。

### 取消与清理

调用被取消（或超出 CPU / 内存预算）时立即记录终态，随后进入有界清理：它等待的宿主操作和不是它自己创建的 Promise 以 `AbortError` 拒绝，它自己创建的叶子 Promise 也被拒绝；每个在取消前已存在的 async 帧恰好收到一次 `AbortError`，因此 `catch`/`finally` 照常运行，`finally` 里的 `await` 也正常工作。清理期间可以调用别的插件的方法、做宿主 IO，但不能发起新的子调用。整段清理有 **2 秒** 墙钟期限，结束或超时后回收所属的定时器、HTTP、宿主等待和排队续体。被强制终止的那段同步代码里的 `finally` 不会执行；已经发生的写入不会回滚。

`--record` 的 `event: "cleanup"` 包含清理 `id`、`realm`、所属 fiber `owner`（`onDispose` 为空）、`status` 和可选的 `error`；状态为 `started` 加一次 `done` / `failed` / `timed_out` / `cancelled`，可能出现在对应调用的 `exit` 之后。

### 审批、TUI 与 Jev

```bash
./target/debug/ply tui --record /tmp/ply-tui.jsonl
```

TUI 把用户输入交给 agent，不直接调用 read / bash 等工具。主 agent 默认有 agent / bash / create / edit / memory / read / remove 七个工具，子 agent 去掉 agent 与 memory。CLI 为 TUI 选择逐次审批策略，读取、写入、命令执行和模型委派都需要确认；有活动会话时由 TUI 回答审批。`ply tui --bypass` 显式跳过人工确认，界面显示 BYPASS，但工具授权、子 agent 上限、挂载限制和普通 hook 的硬拒绝仍然生效。详见 [TUI](rfcs/0005-tui.md)。

其他环境通过审批适配器插件接入，模板见 [examples/plugins/approval-adapter](examples/plugins/approval-adapter/main.ts)（只返回 pending）。尚未得到批准时，agent 以 `status: "approval_required"` 和 `approval_requests` 返回给宿主，不把待审命令当作完成，也不给模型自我批准的入口。协议见 [审批](rfcs/0003-approval.md)。

`--jev` 启用 [Jev 安全检查](rfcs/0010-jev.md)，需要在宿主环境配置 `TYPESAFE_API_KEY`。它独立调用 TypeSafe `/v1/systemone` 批量评估命令风险，按本地阈值给出放行、高风险或复核建议，不更换主 agent 的模型。未配置密钥、失败、高风险或待复核都会请求确认；`--jev` 同时加载审批协调插件。环境模板见 [examples/jev.env.example](examples/jev.env.example)。

### Skills

内置 `skill` 从启动目录下的 `.agents/skills`、`skills` 和 `SKILL.md` 发现技能，并默认加载 `~/.agents/skills`。CLI 为用户 skills 及其直属符号链接技能建立只读挂载；同名技能依次优先显式路径、项目目录、用户目录。额外路径通过插件参数传入，例如 `ply tui --plugin-options 'skill={"paths":["./examples/skills"]}'`。元数据写入上下文的 Extensions 部分，完整指令由模型通过 read 按需读取，详见 [Skills](rfcs/0006-skills.md)。

## 开发

先读 [SPEC.md](SPEC.md)：它是当前的系统契约和验收来源。改变行为、公共接口或存储格式前，先在 [rfcs/](rfcs/README.md) 写 RFC（[模板](rfcs/TEMPLATE.md)），实现后同步 SPEC。开发约定见 [AGENTS.md](AGENTS.md)。

```bash
npm ci --ignore-scripts --registry=https://registry.npmjs.org
npm run check
cargo test --workspace --locked --offline
```

`npm run check` 执行严格类型检查、ESLint、文档链接检查和插件可见面检查，覆盖 SDK 声明、插件、示例与 [类型契约反例](type-tests/plugins.ts)。根 `tsconfig.json` 自动加载 SDK 声明，编辑器打开仓库即可检查所有插件。

克隆后启用提交钩子，它会对已暂存的 TS/JS 自动执行 `eslint --fix`：

```bash
git config core.hooksPath .githooks
```

插件按职责拆分模块：`main.ts` 只放钩子入口，`types.ts` 声明输入、输出和导出接口，例如 [agent](plugins/agent/) 由 run / model / tools / policy / report / recovery / usage 分担实现。每个 TS 文件最多 250 个非空、非注释行。内置插件的模块目录由 Rust 在构建期收集并嵌入二进制。

## 许可证

MIT
