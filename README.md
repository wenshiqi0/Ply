# wendy

一个 agent harness。Rust 是主核心，插件用 TS 写。一个插件最多导出两个钩子：`onHandle(input)` 每次调用运行一次，每次运行是内核调度的一个 fiber；`onLoad()` 在加载时运行一次，返回的东西挂到全局的 `ctx` 上。插件之间不互相调用，只互相等待：`await tools.bash({ command })` 的那一刻就是调用。

wendy 记录的是 fiber：每个 fiber 的输入、运行中主动记录的数据、输出（或错误），以及谁等待了谁。插件只能看到挂载的存储，挂载的存储可以在磁盘、内存、数据库之间切换。

设计细节见 [docs/design.md](docs/design.md)。

## 写一个插件

一个目录就是一个插件，目录名就是插件名，`main.ts` 用具名导出提供钩子：

```ts
// todos/main.ts

/** Count TODO lines in a file. */
export async function onHandle(input: { path: string }): Promise<{ todos: number; lines: number }> {
  const file = JSON.stringify(input.path);
  const [grep, wc] = await Promise.all([
    tools.bash({ command: `grep -c TODO ${file}` }),   // 等待另一个插件：一个子 fiber
    tools.bash({ command: `wc -l < ${file}` }),
  ]);
  return { todos: Number(grep.stdout.trim()), lines: Number(wc.stdout.trim()) };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  await tools.context.loaded;          // 等 context 插件加载完
  await ctx.registerTool("todos");     // 调用它挂在 ctx 上的函数
}
```

- **`onHandle`**：`tools.todos(input)` 被 await 时运行。它上面的 JSDoc 是插件的描述，参数类型和返回类型就是输入和输出（wendy 加载时从签名里读出来）。参数校验由插件自己做；权限这类控制也放在插件里。
- **`onLoad`**：加载时在后台运行。
  - 返回的对象平铺挂到 `ctx` 上：数据是一份拷贝，函数在挂载它的插件里执行。
  - 同名的成员，后挂载的覆盖先挂载的。
  - 可以 `await tools.xxx.loaded` 等别的插件加载完，不受加载顺序影响。
- **共享闭包**：同一个插件的多次执行共享模块里的闭包，在 `await` 处交替执行。
- **可以列举插件**：`Object.keys(tools)` 列出已加载的插件；有 `onHandle` 的插件还带 `tools.x.parameters`，即它作为工具时的参数 schema。
- **node 模块照常用**（`fs`、`path`、`child_process` 等），类型声明见 [packages/sdk/globals.d.ts](packages/sdk/globals.d.ts)。

## 默认插件

| 插件 | 钩子 | 做什么 |
|---|---|---|
| [agent](plugins/agent/main.ts) | onHandle、onLoad | 用语言模型完成一个任务，并把自己注册到 context，允许模型开启子 agent |
| [context](plugins/context/main.ts) | onLoad | 管理模型看到的东西：注册进来的工具、要用的模型、进行中的对话（过长的工具输出会被截短） |
| [deepseek](plugins/deepseek/main.ts) | onLoad | 本机的 DeepSeek 接入：告诉 context 用 `deepseek-v4-flash`，key 在 `DEEPSEEK_API_KEY` |
| [bash](plugins/bash/main.ts) | onHandle、onLoad | 用系统 bash 运行命令，并把自己注册到 context |
| [create](plugins/create/main.ts) | onHandle、onLoad | 新建文件，并把自己注册到 context |
| [edit](plugins/edit/main.ts) | onHandle、onLoad | 精确替换文件里的一段文字，并把自己注册到 context |
| [read](plugins/read/main.ts) | onHandle、onLoad | 从挂载存储读取 UTF-8 文本，支持按行分页，并把自己注册到 context |
| [remove](plugins/remove/main.ts) | onHandle、onLoad | 删除文件或目录，并把自己注册到 context |

context 和 deepseek 没有 `onHandle`，不能被调用，也不会作为工具透出给模型或 MCP 客户端。

读取文件优先用 `read`：`path` 必填，`offset` 是从 1 开始的行号（默认 1），`limit` 是最多返回的行数（默认 200）。返回原始文本 `content`、本页起始行 `offset`、返回行数 `lines`、文件总行数 `total_lines` 和 `has_more`；下一页从 `offset + lines` 开始。空文件返回空文本和 0 行。分页限制返回内容，底层仍一次读取整个文件。

agent 需要本机装有 [pi](https://github.com/earendil-works/pi-coding-agent)（`npm install -g @earendil-works/pi-coding-agent`，或用 `WENDY_PI_AI` 指向一个 pi-ai 包）：

```bash
./target/debug/wendy call --record fibers.jsonl agent '{"prompt": "把 app.py 的输出改成 hello, wendy，然后运行验证"}'
```

agent 每次调用工具都是它的子 fiber，记录里能看到完整的过程。`agent` 本身也是模型可见的工具：模型调用 `agent({ prompt, max_turns? })` 时，会运行一个子 agent，父 agent 等它完成，结果作为工具输出返回；同一轮发起的多个子 agent 按现有工具调度并发运行。

每个子 agent 新建独立对话，只接收传入的 `prompt`，父 agent 要在里面提供任务背景和约束。父子 agent 共用挂载存储、工作目录以及 context 提供的模型和工具。子 agent 可以继续调用 `agent`；递归沿用内核的 fiber 嵌套深度限制（默认 16，所有工具调用都计入），`max_turns` 仍分别限制每个 agent 的模型轮数（默认 20）。

agent 的返回结果同时带两份用量汇总：`usage` 只统计当前 agent 自己的模型调用，`total_usage` 统计当前 agent 和全部子孙 agent。子 agent 的用量逐轮计入祖先，已报告的失败调用也会计入；父 agent 不会重复加上子 agent 返回结果中的汇总。

| usage 字段 | 含义 |
|---|---|
| `calls` / `reported_calls` / `missing_calls` | 模型调用次数 / 有有效 usage 的次数 / 缺失 usage 的次数 |
| `input_tokens` | 全部输入 token，包含未命中、缓存读取和缓存写入 |
| `uncached_input_tokens` | 普通输入 token，不含单独计量的缓存读取/写入，对应 pi-ai 的 `usage.input` |
| `output_tokens` / `reasoning_tokens` | 输出 token / 其中的推理 token；推理量未报告时为 `null` |
| `cache_read_tokens` / `cache_write_tokens` | 缓存读取 / 缓存写入 token |
| `total_tokens` | 输入 + 输出，推理 token 不重复相加 |
| `cache_hit_rate` | 累计缓存读取 token / 累计全部输入 token，范围 0–1；按 token 加权，不平均各轮比例 |
| `complete` | 是否每次模型调用都有有效 usage；为 false 时 token 数是已报告部分的小计，整体命中率为 `null` |

没有 usage 或 SDK 的全零占位 usage 都记为缺失，不当作实测 0 用量；没有输入 token 时命中率为 `null`。这些值来自 pi-ai 的 usage，单次 `complete` 内部的 SDK 重试不会另算一轮。

加上 `--record fibers.jsonl` 可保存每轮详情：新的 `event: "record"` 事件由内核附上 `id` 和 `at_ms`，其 `data.type` 为 `model_call` 时记录轮次、模型、响应 ID、停止原因、耗时、usage 或错误；为 `agent_usage` 时记录该 agent 的自身/子树汇总和完成/失败状态。每轮在拿到响应后、执行工具或处理错误前写出；达到轮数上限或模型调用失败也会写汇总。统计整棵树时，汇总 `model_call` 事件或读取根 agent 的 `total_usage`，不要再叠加各级 `total_usage`。

## 试一下

```bash
cargo build -p wendy-cli
```

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

直接运行一个 TS 文件（像 node 一样，定时器跑完才退出；`tools` 可用）：

```bash
./target/debug/wendy run ./script.ts -- arg1
```

选项：
- `--mount SPEC`：`AT=disk:PATH[:ro]`、`AT=sqlite:PATH[:ro]`、`AT=memory`，或者 `disk:PATH[:ro]`（挂在它自己的绝对路径上），可以重复多次。
- `--plugin DIR`：加载一个插件目录，可以重复多次。
- `--no-default-plugins`：不加载默认插件。
- `--record FILE`：把每个 fiber 事件（spawn / record / exit）以 JSON lines 追加到文件。

## crate

| crate | 内容 |
|---|---|
| `wendy-storage` | 磁盘 / 内存 / SQLite 后端，同一套行为测试同时跑三种 |
| `wendy-io` | 挂载表（mount / unmount / swap，只有挂载的路径存在）；在磁盘挂载上启动真实进程 |
| `wendy-libc` | errno / path / fs / glob / time |
| `wendy-jsc` | 内嵌 JavaScriptCore，直接运行 TS；每个插件一个 realm、一个线程；node API 和全局 `tools`、`ctx` |
| `wendy-core` | 描述层：插件、输入输出的类型、`onLoad` 的挂载、fiber、fiber 事件和 hook |
| `wendy-kernel` | 加载插件（读出签名、后台运行 `onLoad`、`ready`）、调度 fiber、维护 `tools` 和 `ctx`、发出 fiber 事件、默认插件 |
| `wendy-cli` | `wendy tools`、`wendy call`、`wendy mcp`、`wendy run` |
| `wendy-sh` | 暂停：自研的对齐 bash 的 sh，源码保留，不参与构建 |

## 测试

```bash
cargo test --workspace
```

目前只支持 macOS（系统 JavaScriptCore）。
