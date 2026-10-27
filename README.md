# Ply

[![CI](https://github.com/wenshiqi0/Ply/actions/workflows/ci.yml/badge.svg)](https://github.com/wenshiqi0/Ply/actions/workflows/ci.yml)

Ply 是一个 agent harness：Rust 内核负责调度、IO 和记录，插件用 TypeScript 写，运行在内嵌的 JavaScriptCore 里。一个目录就是一个插件，导出的函数签名就是它的工具定义；内置的 agent、上下文、审批和终端界面也都是普通插件。

目前只支持 macOS（使用系统 JavaScriptCore）。

## 特点

- **插件只写能力**：一个插件最多导出 `onHandle`、`onLoad`、`onDispose` 三个钩子；调度、调用归属、取消、清理和加载顺序都在 Rust 内核里，插件代码看不到。
- **调用就是 `await`**：`await plugins.bash({ command })` 的那一刻就是一次调用，并发直接用 `Promise.all`。
- **挂载式 IO**：文件访问通过挂载，后端可在磁盘、内存、SQLite 之间切换；模型的 `bash` 工具由内置的 Ply sh 执行。
- **自带 agent**：分层上下文与自动压缩、子 agent 委派与配额、用量统计、失败后从快照恢复；当前接入 DeepSeek 与 MiMo。
- **审批与安全**：环境审批协议、终端 TUI 审批、可选的 Jev 命令风险审查。
- **多种入口**：CLI、脚本、MCP server、TUI；`--record` 把每次调用的过程写成 JSON lines。

## 快速开始

需要 macOS 和 Rust 工具链。

```bash
cargo build -p ply-cli
```

在宿主环境配置 `DEEPSEEK_API_KEY`，然后：

```bash
./target/debug/ply tui
```

```bash
./target/debug/ply call agent '{"prompt": "只读总结这个项目"}'
```

作为 MCP server 给其他 agent 使用：

```bash
claude mcp add ply -- /path/to/ply/target/debug/ply mcp
```

## 一个插件

```ts
// todos/main.ts
import plugins, { context } from "ply:plugins";

/** Count TODO lines in a file. */
export async function onHandle(input: { path: string }): Promise<{ todos: number }> {
  const result = await plugins.bash({ command: `grep -c TODO ${JSON.stringify(input.path)}` });
  return { todos: Number(result.stdout.trim()) };
}

/** Offer this plugin to the model as a tool. */
export async function onLoad() {
  await context.registerTool("todos");
}
```

```bash
./target/debug/ply call --plugin ./todos todos '{"path": "README.md"}'
```

## 文档

| | |
|---|---|
| [写插件](docs/plugins.md) | 钩子、插件句柄、加载顺序、内置插件、Skills |
| [命令行](docs/cli.md) | 命令、选项、挂载、记录文件 |
| [架构](docs/architecture.md) | crate 分层、fiber 调度、取消与清理 |
| [工具](docs/tools.md) · [模型接入](docs/models.md) · [agent](docs/agent.md) · [上下文](docs/context.md) · [审批与安全](docs/approval.md) | 各子系统的用法与配置 |
| [开发](docs/development.md) | 规范驱动的开发流程、检查命令、提交钩子 |
| [SPEC](SPEC.md) · [RFC](docs/rfcs/README.md) | 当前系统契约 · 设计决策记录 |

完整索引见 [docs/](docs/README.md)。

## 许可证

MIT
