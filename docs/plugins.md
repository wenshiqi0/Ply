# 写插件

一个目录就是一个插件，目录名就是插件名。入口文件按 `main.ts`、`index.ts`、`main.js`、`index.js` 的顺序选择，用具名导出提供钩子。插件只关心能力：自己的输入、调用别的插件得到的返回值、自己持有的值；调度、调用归属、取消和加载顺序都由内核负责，插件代码看不到它们（[RFC 0012](rfcs/0012-capabilities-not-scheduling.md)）。

## 一个例子

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

## 三个钩子

- **`onHandle(input)`**：被 await 时运行一次。它的 JSDoc 是插件描述，参数类型和返回类型就是工具的输入和输出，加载时从签名里读出。参数校验由插件自己负责。
- **`onLoad(options)`**：加载时在后台运行一次，接收宿主为该插件配置的 JSON（未指定时为 `{}`），由插件自己解释。返回的数据和函数通过该插件的句柄访问：数据是一份拷贝，函数在所属插件里执行。
- **`onDispose()`**：内核回收插件 realm 时运行一次（宿主关闭内核，或 `onLoad` 失败之后），两秒期限。每次调用自己的清理写在 `finally` 里：调用被取消时，它在等待的东西以 `AbortError`（`code: "ERR_PLY_CANCELLED"`）拒绝，`catch`/`finally` 照常运行，见 [取消与清理](architecture.md#取消与清理)。

## 插件句柄

- **调用就是 `await`**：`plugins.bash(input)` 是惰性的，被 await 的那一刻才调用；同一个句柄 await 两次只执行一次。并发直接用 `Promise.all`。
- **具名导入决定加载顺序**：用 `import { x } from "ply:plugins"` 具名导入的插件，内核保证它的 `onLoad` 先完成；它加载失败时本插件也加载失败，相互导入成环会报错。默认导入 `import plugins from "ply:plugins"` 得到动态目录，不建立顺序，调用照常等待目标加载完成。
- **列举插件**：`Object.keys(plugins)` 列出已加载的插件；有 `onHandle` 的插件还带 `plugins.x.parameters`，即它作为工具时的参数 schema。
- **共享闭包**：同一个插件的多次调用共享模块里的状态，在 `await` 处交替执行。
- **类型**：已知插件的句柄有具体的参数和返回类型，动态插件返回 unknown，使用前需要收窄。声明入口是 [packages/sdk/index.d.ts](../packages/sdk/index.d.ts)。

## 运行环境

- Node 模块照常用（`fs`、`path`、`child_process` 等），由 JSC shim 提供；Node 类型只用于编辑器，实际支持范围以运行时及其测试为准。
- 插件可以用插件目录内的相对 import 拆分模块，同一插件的模块共享一个 realm 和模块缓存。
- `ply:events` 声明和订阅插件事件，见 [RFC 0002](rfcs/0002-events.md)；`ply:commands` 在 `onLoad` 里注册顶层 CLI 命令，见 [RFC 0004](rfcs/0004-commands.md)。

## 向上下文提供内容

模型上下文统一由 context 插件构建。其他插件通过 `context.set` 提供内容：

```ts
await context.set({ source: "skill", type: context.Type.Extensions, key: "skills", content });
```

`source` 是插件自己的名字；不指定数字 order，也不注册构建回调，类型决定顺序和更新策略。不给 `conversation` 时修改模板，只影响之后打开的会话。详见 [上下文](context.md) 和 [RFC 0007](rfcs/0007-context.md)。

## 内置插件

默认加载 DeepSeek 接入；用 `--model mimo-v2.6-pro` 或 `--model mimo-v2.6-flash` 切换为 MiMo，两者只加载一个。

| 插件 | 钩子 | 做什么 |
|---|---|---|
| [agent](../plugins/agent/main.ts) | onHandle、onLoad | 用语言模型完成一个任务，并把自己注册到 context，允许模型开启子 agent，见 [agent](agent.md) |
| [context](../plugins/context/main.ts) | onLoad | 按 Rules/Project/Extensions/State/Reference 管理上下文，冻结前缀并追加状态更新，负责工具快照、预算、压缩和恢复 |
| [deepseek](../plugins/deepseek/main.ts) | onLoad | DeepSeek 接入：告诉 context 使用 `deepseek-v4-flash`，密钥在宿主的 `DEEPSEEK_API_KEY` |
| [mimo](../plugins/mimo/main.ts) | onLoad | 可选的 MiMo V2.6 接入，密钥在宿主的 `MIMO_API_KEY`，见 [模型接入](models.md) |
| [bash](../plugins/bash/main.ts) | onHandle、onLoad | 用内置 Ply sh 执行脚本；执行前等待 `bash.beforeExecute` 事件，见 [工具](tools.md) |
| [read](../plugins/read/main.ts) | onHandle、onLoad | 从挂载存储读取 UTF-8 文本，支持按行分页 |
| [create](../plugins/create/main.ts) | onHandle、onLoad | 新建文件 |
| [edit](../plugins/edit/main.ts) | onHandle、onLoad | 精确替换文件里的一段文字 |
| [remove](../plugins/remove/main.ts) | onHandle、onLoad | 删除文件或目录 |
| [memory](../plugins/memory/main.ts) | onHandle、onLoad | 显式记住、跨会话召回和遗忘；私有文件存储，由 context 注入，见 [RFC 0008](rfcs/0008-memory.md) |
| [skill](../plugins/skill/main.ts) | onLoad | 发现 SKILL.md 元数据并注入上下文，见下文 |
| [approval](../plugins/approval/main.ts) | onLoad | 可选的审批策略与协调，见 [审批与安全](approval.md) |
| [tui](../plugins/tui/main.ts) | onHandle、onLoad | 可选的终端交互与审批界面；只调用 agent |
| [jev](../plugins/jev/main.ts) | onLoad | 可选的 TypeSafe Jev 安全检查，订阅 `bash.beforeExecute`；用 `--jev` 启用 |

context、deepseek、mimo 等没有 `onHandle` 的插件不能被调用，也不会作为工具透出给模型或 MCP 客户端。

## Skills

内置 `skill` 从启动目录下的 `.agents/skills`、`skills` 和 `SKILL.md` 发现技能，并默认加载 `~/.agents/skills`。CLI 为用户 skills 及其直属符号链接技能建立只读挂载；同名技能依次优先显式路径、项目目录、用户目录。额外路径通过插件参数传入：

```bash
./target/debug/ply tui --plugin-options 'skill={"paths":["./examples/skills"]}'
```

元数据写入上下文的 Extensions 部分，完整指令由模型通过 read 按需读取，不扩大工具、审批或委派权限。详见 [RFC 0006](rfcs/0006-skills.md)。

## 示例

[examples/plugins](../examples/plugins/) 里有：bash 执行前的 hook（bash-guard）、审批环境适配器模板（approval-adapter）、注册顶层命令（hello-command）。
