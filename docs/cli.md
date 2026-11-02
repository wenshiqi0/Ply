# 命令行

```bash
cargo build -p ply-cli
```

## 命令

| 命令 | 做什么 |
|---|---|
| `ply tools` | 列出已加载插件及其工具 schema（JSON） |
| `ply call PLUGIN JSON` | 调用一个插件并输出结果 |
| `ply run FILE.ts -- ARGS` | 像 node 一样运行一个 TS 文件，定时器跑完才退出；通过 `ply:plugins` 导入插件目录 |
| `ply acp` | 由 ACP 插件注册，以 stdio 提供 agent 会话 |
| `ply tui` | 交互任务与终端审批，见 [审批与安全](approval.md) |
| `ply commands` | 列出插件注册的顶层命令（JSON），例如 `ply memory` |

```bash
./target/debug/ply tools
./target/debug/ply call read '{"path": "README.md", "offset": 1, "limit": 80}'
./target/debug/ply call --record fibers.jsonl edit '{"path": "src/main.rs", "old_string": "hi", "new_string": "hello"}'
./target/debug/ply run ./script.ts -- arg1
```

通过 ACP 连接支持 ACP v1 的客户端：

```bash
/path/to/ply/target/debug/ply acp --record /tmp/ply-events.jsonl
```

ACP 插件按行处理 JSON-RPC 2.0，提供会话、图片提示、模型/思考配置、公开回复、工具进度、双向权限请求及会话取消。会话私有状态保存在 `~/.ply/plugins/acp`，工作目录必须与进程挂载目录一致；本版不接入外部 MCP servers。旧 `ply mcp` 与 `--host-chat` 已移除，客户端须改为 ACP。协议入口不成为模型工具；实现与边界见 [RFC 0019](rfcs/0019-acp-plugin.md)。

## 常用选项

| 选项 | 含义 |
|---|---|
| `--mount SPEC` | `AT=disk:PATH[:ro]`、`AT=sqlite:PATH[:ro]`、`AT=memory`，或 `disk:PATH[:ro]`（挂在它自己的绝对路径上）；可以重复。默认把当前目录以读写方式挂在它自己的路径上 |
| `--plugin DIR` | 加载一个插件目录，可以重复 |
| `--no-default-plugins` | 不加载内置插件 |
| `--plugin-options NAME=JSON` | 把 JSON 传给对应插件的 `onLoad`，由插件解释 |
| `--model MODEL` | 选择 `deepseek-*` 或 `mimo-*` 模型，见 [模型接入](models.md) |
| `--mimo-provider PROVIDER` | MiMo 的服务或地区，需同时指定 `--model mimo-*` |
| `--jev` | 启用 Jev bash 安全检查 |
| `--record FILE` | 把每个调用事件以 JSON lines 追加到文件，见下文 |

agent 与上下文相关的选项（`--agent-max-children`、`--agent-report-section`、`--context-config`、`--max-context-tokens` 等）见 [agent](agent.md) 和 [上下文](context.md)；`ply tui --bypass` 见 [审批与安全](approval.md)。

## 挂载

文件 API 只能看到挂载的路径。后端可以是磁盘、内存或 SQLite，三者行为一致；只读挂载拒绝写入。内部工具和重定向走挂载；宿主程序保留真实的宿主文件权限，挂载不是 OS 沙箱。

## 记录文件

`--record FILE` 把内核发出的每个事件写成一行 JSON：

| `event` | 含义 |
|---|---|
| `spawn` | 一次调用开始：id、父调用、插件、输入；事件回调和命令带 `event` 字段 |
| `plugin` | 插件发出的事件：`name`、`plugin`、发出它的调用 `fiber`、`data` |
| `exit` | 调用结束：`done`、`failed`、`cancelled` 或 `budget_exceeded`（CPU / 内存），以及耗时 |
| `cleanup` | 取消后的清理或 `onDispose`：`owner`、`started` 加一次终态，见 [架构](architecture.md#取消与清理) |

agent 的模型调用、上下文压缩、检查点等记录都是 `plugin` 事件，见 [agent](agent.md#用量统计)。
