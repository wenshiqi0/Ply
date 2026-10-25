# wendy

一个 agent harness。Rust 是主核心，插件用 TS 写。一个插件最多导出两个钩子：`onHandle(input)` 每次调用运行一次，每次运行是内核调度的一个 fiber；`onLoad()` 在加载时运行一次，返回的东西挂到全局的 `ctx` 上。通过 `import plugins from "wendy:plugins"` 获取插件目录；`await plugins.bash({ command })` 的那一刻就是调用。CLI 入口通过 `wendy:commands` 在 onLoad 注册，见 [命令与模块接口](docs/commands.md)。

wendy 记录的是 fiber：每个 fiber 的输入、运行中主动记录的数据、输出（或错误），以及谁等待了谁。文件 API 通过挂载访问存储，后端可以在磁盘、内存、数据库之间切换；系统进程保留宿主权限，挂载不等于 OS 沙箱。

设计细节见 [docs/design.md](docs/design.md)，当前能力及验收范围见 [docs/capabilities.md](docs/capabilities.md)，插件事件和 bash hook 见 [docs/plugin-events.md](docs/plugin-events.md)，Jev 安全订阅插件见 [docs/jev.md](docs/jev.md)，环境审批协议见 [docs/approval.md](docs/approval.md)。

## 写一个插件

一个目录就是一个插件，目录名就是插件名，`main.ts` 用具名导出提供钩子：

```ts
import plugins from "wendy:plugins";
// todos/main.ts

/** Count TODO lines in a file. */
export async function onHandle(input: { path: string }): Promise<{ todos: number; lines: number }> {
  const file = JSON.stringify(input.path);
  const [grep, wc] = await Promise.all([
    plugins.bash({ command: `grep -c TODO ${file}` }),   // 等待另一个插件：一个子 fiber
    plugins.bash({ command: `wc -l < ${file}` }),
  ]);
  return { todos: Number(grep.stdout.trim()), lines: Number(wc.stdout.trim()) };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  await plugins.context.loaded;          // 等 context 插件加载完
  await ctx.registerTool("todos");     // 调用它挂在 ctx 上的函数
}
```

- **`onHandle`**：`plugins.todos(input)` 被 await 时运行。它上面的 JSDoc 是插件的描述，参数类型和返回类型就是输入和输出（wendy 加载时从签名里读出来）。参数校验由插件负责；agent 权限由插件收窄，fiber 归属与子任务配额由内核执行。
- **`onLoad`**：加载时在后台运行。
  - 返回的对象平铺挂到 `ctx` 上：数据是一份拷贝，函数在挂载它的插件里执行。
  - 同名的成员，后挂载的覆盖先挂载的。
  - 可以 `await plugins.xxx.loaded` 等别的插件加载完，不受加载顺序影响。
- **共享闭包**：同一个插件的多次执行共享模块里的闭包，在 `await` 处交替执行。
- **可以列举插件**：`Object.keys(plugins)` 列出已加载的插件；有 `onHandle` 的插件还带 `plugins.x.parameters`，即它作为工具时的参数 schema。
- **node 模块照常用**（`fs`、`path`、`child_process` 等），类型声明见 [packages/sdk/globals.d.ts](packages/sdk/globals.d.ts)。

## 内置插件

默认加载 DeepSeek 接入；用 `--model mimo-v2.6-pro` 或 `--model mimo-v2.6-flash` 切换为 MiMo，只加载一个模型接入插件。

| 插件 | 钩子 | 做什么 |
|---|---|---|
| [agent](plugins/agent/main.ts) | onHandle、onLoad | 用语言模型完成一个任务，并把自己注册到 context，允许模型开启子 agent |
| [context](plugins/context/main.ts) | onLoad | 管理模型看到的东西：注册进来的工具、要用的模型、进行中的对话（过长的工具输出会被截短） |
| [deepseek](plugins/deepseek/main.ts) | onLoad | 本机的 DeepSeek 接入：告诉 context 用 `deepseek-v4-flash`，key 在 `DEEPSEEK_API_KEY` |
| [approval](plugins/approval/main.ts) | onLoad | 可选的审批策略与协调插件；通过 approval.request / toolRequest 对接环境适配器，不自行批准 |
| [tui](plugins/tui/main.ts) | onHandle、onLoad | 可选的终端交互与工具审批界面；只调用 agent，工具集和委派由 agent 管理 |
| [jev](plugins/jev/main.ts) | onLoad | 可选的 TypeSafe Jev 原生安全检查器，订阅 bash.beforeExecute；用 --jev 启用，密钥在宿主 TYPESAFE_API_KEY |
| [mimo](plugins/mimo/main.ts) | onLoad | 可选的 MiMo V2.6 接入，key 在宿主的 `MIMO_API_KEY`，支持 Token Plan 和按量付费 API |
| [bash](plugins/bash/main.ts) | onHandle、onLoad | 用内部 Wendy sh 执行脚本；执行前等待 bash.beforeExecute，并把自己注册到 context |
| [create](plugins/create/main.ts) | onHandle、onLoad | 新建文件，并把自己注册到 context |
| [edit](plugins/edit/main.ts) | onHandle、onLoad | 精确替换文件里的一段文字，并把自己注册到 context |
| [read](plugins/read/main.ts) | onHandle、onLoad | 从挂载存储读取 UTF-8 文本，支持按行分页，并把自己注册到 context |
| [remove](plugins/remove/main.ts) | onHandle、onLoad | 删除文件或目录，并把自己注册到 context |

fiber 的归属由消费方的异步续体携带，共享 Promise 后也保持独立父子关系。异常、取消和 CPU 超限按所属调用隔离；Rust 可用 `Kernel::cancel_fiber(id)` 请求取消，父任务取消会传播到等待中的子任务。JSONL 的结束状态区分 `done`、`failed`、`cancelled` 和 `budget_exceeded`（CPU/内存）；独立的 `cleanup` 事件记录清理归属和结果。同插件仍共享闭包及线程，内存阈值仍是全进程水位。详见 [fiber 调度与隔离](docs/design.md#4-同一个插件的多个-fiber)。

context、deepseek 和 mimo 没有 `onHandle`，不能被调用，也不会作为工具透出给模型或 MCP 客户端。

模型可见的 `bash` 工具统一使用 **Wendy sh**。保留 `command` / `cwd` / `timeout_ms` 入参及原有输出字段；每次调用新建 shell，脚本内的状态和 cwd 独立。支持内部解释器的语法、builtin 和自带工具，未注册命令返回 127，不自动调用宿主程序或系统 shell。只读挂载、内存/SQLite 存储也对 shell 生效；其他插件的 `node:child_process` 仍按其自身接口工作。支持范围及缓冲管道限制见 [wendy-sh](crates/wendy-sh/README.md)。

`bash.beforeExecute` 覆盖整个工具调用，所有监听器异步完成且未拒绝后才进入 sh，包含重定向和命令替换在内的脚本内容都尚未执行。普通 hook 返回 `false` / `{allow:false, reason}` 是硬拒绝；带 requires_approval:true 的风险结果进入环境审批。批准后继续，缺少适配器时返回 approval_required。`timeout_ms` 包含检查与执行时间，审批等待单独计时。Wendy sh 没有 shell 进程信号，因此 `signal` 为 null，超时仍通过 `timed_out` 表示。

可用 `--jev` 启用 [Jev 安全检查](docs/jev.md)，自动审查需在宿主环境配置 `TYPESAFE_API_KEY`。它独立调用 TypeSafe `/v1/systemone`，批量评估命令风险概率并按本地阈值生成放行/高风险/复核建议，不更换主 agent 模型。未配置密钥、失败、高风险或待复核会请求确认/审批；可用 [wendy tui](docs/tui.md) 进行真实终端确认，其他入口没有适配器时返回 approval_required。默认不加载 Jev；--jev 同时加载通用审批协调插件。环境模板见 [examples/jev.env.example](examples/jev.env.example)。

读取文件优先用 `read`：`path` 必填，`offset` 是从 1 开始的行号（默认 1），`limit` 是最多返回的行数（默认 200）。返回原始文本 `content`、本页起始行 `offset`、返回行数 `lines`、文件总行数 `total_lines` 和 `has_more`；下一页从 `offset + lines` 开始。空文件返回空文本和 0 行。分页限制返回内容，底层仍一次读取整个文件。

agent 在自己的 JSC realm 内运行内嵌的 [pi-ai](https://github.com/earendil-works/pi/tree/main/packages/ai) 模型层，当前支持 DeepSeek 和 MiMo。运行时无需 Node、npm 或全局安装 Pi；配置宿主的 `DEEPSEEK_API_KEY` 即可使用默认 DeepSeek：

```bash
./target/debug/wendy call --record fibers.jsonl agent '{"prompt": "把 app.py 的输出改成 hello, wendy，然后运行验证"}'
```

MiMo 默认使用中国区 Token Plan（`xiaomi-token-plan-cn`，`https://token-plan-cn.xiaomimimo.com/v1`），密钥只从宿主的 `MIMO_API_KEY` 读取，不注入插件的 `process.env` 或日志。先在终端配置对应服务的密钥，再选择模型：

```bash
./target/debug/wendy call --model mimo-v2.6-pro --record mimo-fibers.jsonl agent '{"prompt": "只读总结这个项目，不修改文件"}'
./target/debug/wendy call --model mimo-v2.6-flash agent '{"prompt": "只读检查 README 中的命令是否与实现一致"}'
```

`--mimo-provider` 可选 `xiaomi`（按量付费）、`xiaomi-token-plan-cn`、`xiaomi-token-plan-sgp`、`xiaomi-token-plan-ams`。每种服务都从 `MIMO_API_KEY` 取密钥，但按量密钥与 Token Plan 密钥不能互换。例如按量 API：

```bash
./target/debug/wendy call --model mimo-v2.6-pro --mimo-provider xiaomi agent '{"prompt": "只读总结这个项目"}'
```

`mimo-v2.6-pro-ultraspeed` 仅在 `--mimo-provider xiaomi` 下开放。MiMo 默认开启思考，历史思考内容随工具调用一起透传；token、推理量和缓存命中率沿用 agent 的统一记录。Token Plan 的 SDK cost 元数据为占位值，不能解读为免费，也不能当作套餐扣减金额；Wendy 不把它输出为实际费用。

MiMo 当前型号与协议依据 [官方型号说明](https://mimo.mi.com/docs/en-US/quick-start/model)、[API 文档](https://mimo.mi.com/docs/en-US/api/chat) 和 [套餐支持说明](https://mimo.mi.com/docs/en-US/quick-start/faq/token-plan/Plans%26Pricing)，定义核对日期为 2026-10-24。V2.5 模型 ID 已退役，不做旧型号替换。MiMo 插件提供显式 V2.6 模型定义。`--no-default-plugins` 下可通过 `--plugin ./plugins/mimo` 手动加载；不要同时加载 deepseek 和 mimo。

模型请求通过 Rust 的 HTTP/TLS 客户端发送，SSE 响应按需逐块读取并交给 JSC 中的 pi-ai 解析；每次调用有独立的取消信号和 10 分钟总超时。HTTP 请求体和累计响应体各限 16 MiB；失败、取消或 realm 退出会释放响应。密钥在请求时从宿主读取，不加入插件 `process.env`、context 或用量记录。浏览器依赖使用私有作用域，不改变普通插件的 Node 风格全局对象。

内嵌 bundle 随源码保存，普通 `cargo build` 不运行 Node。只有更新 SDK/bundle 时需要构建期 Node：

```bash
cd packages/pi-ai
npm ci --ignore-scripts --registry=https://registry.npmjs.org
npm run build
```

依赖锁在 `packages/pi-ai/package-lock.json`，pi-ai 固定为 `0.82.1`；生成文件及第三方声明在 `crates/wendy-jsc/src/vendor/`。`WENDY_PI_AI` 和运行时外部 Node 桥已移除。OAuth、Bedrock 和其他 provider 未接入本轮运行时。

agent 每次调用工具都是它的子 fiber，记录里能看到完整的过程。`agent` 本身也是模型可见的工具：模型调用 `agent({ prompt, allowed_tools?, max_delegation_depth?, max_children?, report_sections?, max_turns? })` 时，会运行一个子 agent，父 agent 等它完成，结果作为工具输出返回；同一轮发起的多个子 agent 按现有工具调度并发运行。

每个子 agent 新建独立对话，只接收传入的 `prompt`，父 agent 要在里面提供任务背景和约束。父子 agent 共用挂载存储、工作目录及模型。每个会话有独立工具白名单，模型可见定义、token 预算和执行前检查使用同一份权限；子 agent 只能收窄父级授权。`max_delegation_depth` 表示当前 agent 还能创建多少代子任务，主 agent 默认 1，子任务继承后减 1；到 0 时移除 `agent` 工具，并在入口/执行层拒绝继续委派。更深递归须由主调用显式授予，还受内核 fiber 深度限制（默认 16，工具调用也计入）。默认不限制模型轮数，直到模型完成任务、最终报告通过完整性校验；只有显式传入正整数 `max_turns` 时，才分别限制对应 agent 的轮数。

只读、仅允许一层子任务的例子（工具白名单仍需配合可信插件使用，不等于 OS 沙箱）：

```bash
./target/debug/wendy call agent '{"prompt":"启动两个子 agent 只读评审项目，再汇总", "allowed_tools":["read","agent"], "max_delegation_depth":1, "max_children":2, "report_sections":["架构判断","发现","验证边界"]}'
```

`allowed_tools` 省略时取已注册工具的快照；子任务继承并与父权限取交集，显式请求父级没有的工具或更深委派会报错。`max_delegation_depth:0` 禁止委派，`2` 允许孙任务。使用提示词限制工作范围时，仍应同时设置这些结构化权限。

`max_children` 是每个 agent 累计创建直属子 agent 的硬上限，主调用默认 **2**；不是并发数，也不限制读取文件等普通工具。内核在同一把锁下检查名额并预占，超额请求在创建子 fiber 前拒绝。已启动后失败、完成、取消以及替换/恢复调用都不退还名额；拒绝创建的请求不占新名额。`0` 会移除 `agent` 工具；子级继承父级的上限并只能收窄，和代数限制分别生效。返回值的 `delegation` 给出 `limit`、`started`、`remaining`。

CLI 的 `--agent-max-children COUNT` 设置主调用默认值，主调用 JSON 中的 `max_children` 优先。Rust 宿主可通过 `KernelConfig.env["WENDY_AGENT_MAX_CHILDREN"]` 设置同一默认值。模型轮数仍默认不限。

最终报告必须正常停止、非空、代码围栏闭合，正文不得残留 DSML/工具控制标记；代码中的协议示例允许保留。内部使用独立结束标记 `[[WENDY_REPORT_COMPLETE]]`，返回前删除。`report_sections` 可要求指定 Markdown 标题及非空内容；省略时只检查通用完整性。标题要求自动传给子 agent，子级和恢复调用只能增加要求。CLI 可重复传 `--agent-report-section HEADING`，Rust 配置为 `KernelConfig.env["WENDY_AGENT_REPORT_SECTIONS"]` 的 JSON 字符串数组。

校验失败写 `agent_report_rejected`，在原 agent 内继续生成完整报告，不创建替代 agent，不把有问题的原文放回模型历史；后续模型调用照常计入 usage。通过后写 `agent_report_validated`，返回 `report_validation`（要求的章节及累计拒绝次数）。这是格式和完成状态校验，不证明结论正确或证据充分；默认不限制修复轮数。带半截工具调用的 `length` 响应仍立即失败并保留可恢复状态，不执行部分参数。

agent 的返回结果同时带两份用量汇总：`usage` 只统计当前 agent 自己的模型调用，`total_usage` 统计当前 agent 子树的模型调用，包括子 agent 及 Jev 等插件报告的调用。子 agent 的用量逐轮计入祖先，已报告的失败调用也会计入；父 agent 不会重复加上子 agent 返回结果中的汇总。

| usage 字段 | 含义 |
|---|---|
| `calls` / `reported_calls` / `missing_calls` | 模型调用次数 / 有有效 usage 的次数 / 缺失 usage 的次数 |
| `input_tokens` | 全部输入 token，包含未命中、缓存读取和缓存写入 |
| `uncached_input_tokens` | 普通输入 token，不含单独计量的缓存读取/写入，对应 pi-ai 的 `usage.input`；原生 API 未报告分项时为 null |
| `output_tokens` / `reasoning_tokens` | 输出 token / 其中的推理 token；推理量未报告时为 `null` |
| `cache_read_tokens` / `cache_write_tokens` | 缓存读取 / 缓存写入 token；供应商未报告分项时为 null |
| `total_tokens` | 输入 + 输出，推理 token 不重复相加 |
| `cache_hit_rate` | 累计缓存读取 token / 累计全部输入 token，范围 0–1；按 token 加权，不平均各轮比例；缓存分项未知时为 null |
| `complete` | 是否每次模型调用都有有效输入/输出 usage；为 false 时是已报告部分的小计。完整 totals 也可能缺少缓存分项，此时命中率为 null |

没有 usage 或 SDK 的全零占位 usage 都记为缺失，不当作实测 0 用量；没有输入 token 时命中率为 `null`。这些值来自 pi-ai 的 usage，单次 `complete` 内部的 SDK 重试不会另算一轮。

每次付费模型调用前先记录 `model_call_started`，拿到响应或异常后记录对应 `model_call`，用同一 fiber ID 与 `call_index` 关联。强制取消可能只留下 started：该次用量未知，不能解释为 0 token；完整结束的调用仍按原有 usage 汇总。

加上 `--record fibers.jsonl` 可保存每轮详情：新的 `event: "record"` 事件由内核附上 `id` 和 `at_ms`，其 `data.type` 为 `model_call` 时记录轮次、模型、响应 ID、停止原因、耗时、usage 或错误；为 `agent_usage` 时记录该 agent 的自身/子树汇总和完成/失败状态。每轮在拿到响应后、执行工具或处理错误前写出；达到轮数上限或模型调用失败也会写汇总。统计整棵树时，汇总 `model_call` 事件或读取根 agent 的 `total_usage`，不要再叠加各级 `total_usage`。

`onDispose` 清理在执行第一条回调前就启动 **2 秒墙钟期限**，所有回调共享该期限。超时直接设置同一取消信号，CPU watchdog（约 100 ms 检查）和同步 fs/exec 等待可观察到取消，随后撤销所属定时器、HTTP、宿主等待和排队续体，阻止超时后的成功返回。清理调用排入下一次 realm 调度，避免嵌套在失败调用的 JS 栈中执行。回调先整批移出注册表；单条异常会继续执行后续回调并将整批标为失败，超时则停止剩余回调，禁止清理中再次注册清理。整批完成时同样撤销遗留的清理定时器、宿主等待及续体，未 await 的清理工作不能继续占用 realm；普通成功调用仍保留原有的 Node 风格定时器行为。

`--record` 的 `event:"cleanup"` 包含清理调用 `id`、`realm`、所属 fiber `owner`、`status` 和可选 `error`；状态为 `started` 及一次 `done` / `failed` / `timed_out` / `cancelled`。它与 fiber 的 `exit` 独立，可能在其后出现。截止表示请求取消并收回 Wendy 的等待与续体，JSC 中断存在检查间隔；已经发生的写入和不能中止的 OS 系统调用不会回滚。

## 上下文预算与压缩

默认总窗口为 **131072 token**，其中预留 **16384 token** 给输出，可用输入预算为 **114688 token**。配置窗口还会受模型目录或显式模型定义的真实窗口约束；输出预留同时是每次任务回复的 `maxTokens`，不会超过模型的最大输出。窗口约束单次模型请求，不限制累计 token 或模型轮数；每个主、子 agent 会话独立应用同一配置。

```bash
./target/debug/wendy call --context-config examples/context.json --record fibers.jsonl agent '{"prompt":"完整评审这个项目"}'

./target/debug/wendy call --max-context-tokens 262144 --context-reserve-tokens 16384 --context-compact-trigger 0.8 --context-compact-target 0.5 agent '{"prompt":"继续长任务"}'
```

[examples/context.json](examples/context.json) 的字段：

| 配置 | 默认值 | 含义 |
|---|---:|---|
| `max_context_tokens` | 131072 | 输入与预留输出合计的窗口 |
| `reserve_output_tokens` | 16384 | 输出预留与任务请求的输出上限 |
| `compaction.enabled` | true | 是否自动摘要压缩；关闭后仍执行输入预算检查 |
| `compaction.trigger_ratio` | 0.8 | 输入估算达到可用输入预算的这一比例时触发 |
| `compaction.target_ratio` | 0.5 | 本次压缩希望达到的输入预算比例，必须小于触发比例 |
| `compaction.keep_recent_turns` | 4 | 原样保留的最近完整交互数，一个工具批次及其全部结果算一组 |
| `compaction.summary_max_tokens` | 2048 | 摘要请求的输出上限，不得大于输出预留 |
| `compaction.max_retries` | 1 | 每个摘要失败后的额外重试次数，范围 0–10 |
| `compaction.retry_delay_ms` | 500 | 重试间隔，范围 0–60000 ms |
| `compaction.failure_cooldown_turns` | 2 | 预算允许延后时，暂停软压缩的任务轮数，范围 0–100 |

显式 CLI 参数覆盖 JSON 文件，未配置项使用默认值。`--context-compaction off` 关闭自动压缩。JSON 中未知字段、非正整数、无效比例及矛盾预算会报错；不自动读取当前目录中的配置文件。Rust 宿主可把同样的 JSON 写入 `KernelConfig.env["WENDY_CONTEXT_CONFIG"]`。

每次模型调用前检查预算。第一次按 UTF-8 字节数/2 加消息与工具定义开销作保守估算；收到有效 usage 后，用该次实际输入+输出作为历史前缀锚点，只估算后续新增内容。**这是估算保护，不是供应商 tokenizer 的精确计数**；记录会标明估算来源、有效窗口、输入预算、触发值和目标值。

达到阈值后，用当前模型把较早的完整交互合并成摘要，保留原始任务、已有摘要中的有效事实以及最近交互。摘要请求不提供工具，省略额外思考配置，并限制摘要输出。过长历史分成多个不拆工具调用的摘要批次，每个批次都检查输入预算；主、子 agent 使用同一配置，各自独立压缩。目标可能因必须保留的原始任务或最近交互而无法完全达到；若仍在预算内且预计压缩无收益，或完整交互无法放入摘要请求，则延后并记录原因。超过输入硬预算则明确报错，不发送超预算的任务请求。

摘要生成、内容校验、上下文版本核对成功后才替换旧历史。空摘要、截断摘要、工具调用、失败响应都保留旧历史并有限重试。重试耗尽但仍在输入硬预算内时，延后软压缩并继续任务；冷却期内超过硬预算仍必须重新压缩，不发送超预算请求。

无法继续时，在宿主私有目录 `$HOME/.local/state/wendy/checkpoints/` 保存原任务、已完成交互、摘要、模型配置、权限、已消耗的直属子 agent 名额、报告校验状态和下一轮位置，错误及 `agent_checkpoint` 事件会给出 `resume_id`。`WENDY_CHECKPOINT_DIR` 可指定其他绝对路径；目录权限 0700、文件 0600，写入采用临时文件 + fsync + 原子替换。快照可能含任务/文件内容，不包含宿主 API key；它不依赖工作区可写权限。

```bash
./target/debug/wendy call agent '{"resume_id":"错误信息中的恢复 ID"}'
```

**快照格式已升级为 2；旧格式明确拒绝恢复，需发起新任务，不进行自动迁移。** 恢复时须使用相同工作目录，会重算当前窗口预算、重新检查工具权限，不能扩大原授权；已花费的子 agent 名额和报告校验状态保留，不能通过恢复重置。恢复记录有跨进程互斥锁，成功后删除快照；工具执行到一半的状态不自动重放。恢复过程在执行工具前标记 `tools_pending`，工具结果全部保存后才回到 `ready`，避免崩溃后重复有副作用的调用。原始模型轮数延续，但 `max_turns` 限制的是这次调用新增轮数；usage/call_index 按本次 fiber 重新记录，历史累计以 `previous_usage` / `previous_total_usage` 单列，不重复累加进祖先。持久化失败会明确报告。单个不可拆交互过大时，仍需减少工具输出或提高预算。

`model_call.purpose` 区分 `task`、`compaction` 与插件的 `safety` 审查，`call_index` 是发起调用的 fiber 内序号。agent 的任务/摘要计入自身及子树，Jev 审查记录在其回调 fiber 并计入祖先 `total_usage`；`turn` 是对应调用的循环轮次，`usage.calls` 包含摘要调用，因此两者可能不同。`context_compaction` 记录压缩成功/失败、重试、前后估算 token、替换消息数、目标是否达到及耗时；`context_compaction_deferred` 记录失败后的冷却与原因。压缩改变历史前缀，可能降低后续缓存复用率，缓存命中率仍以真实上报值累计。

## 试一下

```bash
cargo build -p wendy-cli
```

交互任务与终端权限审批：

```bash
./target/debug/wendy tui --record /tmp/wendy-tui.jsonl
```

TUI 将用户输入交给 agent，不直接调用 read/bash 等业务工具，也不额外传工具白名单。主 agent 默认支持六个工具，子 agent 按现有委派限制移除 agent。CLI 为 TUI 选择逐次审批策略，读取、写入、命令执行和模型委派均需确认；可加 `--jev` 显示 bash 风险评估。详见 [TUI 使用说明](docs/tui.md)。

默认把当前目录以读写方式挂在它自己的路径上：

```bash
./target/debug/wendy tools
```

```bash
./target/debug/wendy call read '{"path": "README.md", "offset": 1, "limit": 80}'
```

```bash
./target/debug/wendy call --record fibers.jsonl edit '{"path": "src/main.rs", "old_string": "hi", "new_string": "hello"}'
```

```bash
./target/debug/wendy call --plugin ./todos todos '{"path": "README.md"}'
```

作为 MCP server 给 agent 用（例如 Claude Code）：

```bash
claude mcp add wendy -- /path/to/wendy/target/debug/wendy mcp --record /tmp/wendy-fibers.jsonl
```

直接运行一个 TS 文件（像 node 一样，定时器跑完才退出；通过 `wendy:plugins` 导入插件目录）：

```bash
./target/debug/wendy run ./script.ts -- arg1
```

选项：
- `--mount SPEC`：`AT=disk:PATH[:ro]`、`AT=sqlite:PATH[:ro]`、`AT=memory`，或者 `disk:PATH[:ro]`（挂在它自己的绝对路径上），可以重复多次。
- `--plugin DIR`：加载一个插件目录，可以重复多次。
- `--jev`：启用 Jev bash 安全订阅插件，配置见 docs/jev.md。
- `--model MODEL`：选择 `deepseek-*` 或 `mimo-*` 模型；使用默认插件时自动选择对应接入，未指定则保持 DeepSeek。
- `--mimo-provider PROVIDER`：MiMo 的服务/地区，需同时指定 `--model mimo-*`，默认中国区 Token Plan。
- `--no-default-plugins`：不加载默认插件。
- `--record FILE`：把每个 fiber 事件（spawn / record / exit / cleanup / plugin）以 JSON lines 追加到文件。

## crate

| crate | 内容 |
|---|---|
| `wendy-storage` | 磁盘 / 内存 / SQLite 后端，同一套行为测试同时跑三种 |
| `wendy-io` | 挂载表（mount / unmount / swap，只有挂载的路径存在）；在磁盘挂载上启动真实进程 |
| `wendy-libc` | errno / path / fs / glob / time |
| `wendy-jsc` | 内嵌 JavaScriptCore，直接运行 TS；每个插件一个 realm、一个线程；Node 风格 API、wendy 模块和 ctx/fiber |
| `wendy-core` | 描述层：插件、输入输出的类型、`onLoad` 的挂载、fiber、fiber 事件和 hook |
| `wendy-kernel` | 加载插件（读出签名、后台运行 `onLoad`、`ready`）、调度 fiber、维护插件目录/ctx/命令注册表、发出 fiber 事件、默认插件 |
| `wendy-cli` | tools/call/mcp/run、commands、插件注册的顶层命令，内置 wendy tui |
| `wendy-sh` | 内部 shell 解释器及工具集，通过 Wendy IO 执行模型 bash tool 的脚本 |

## 测试

```bash
cargo test --workspace
```

目前只支持 macOS（系统 JavaScriptCore）。


审批环境按插件接入：真实终端使用 [TUI 插件](docs/tui.md)，其他环境模板为 [examples/plugins/approval-adapter](examples/plugins/approval-adapter/main.ts)，模板只返回 pending。尚未得到批准时，agent 以 status:approval_required 和 approval_requests 向宿主返回，不把待审命令当作完成，也不暴露模型自我批准入口。
