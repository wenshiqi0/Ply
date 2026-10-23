# wendy

一个 agent harness。Rust 是主核心，插件用 TS 写，插件就是工具的封装；每一次工具调用都是内核里的一次 IPO 操作（input → process → output）。

wendy 管控的是 **IO**。插件（JSC 沙箱，对齐 node）的文件 IO 和它启动的真实进程（系统 bash）都走同一层：只有挂载的存储可见，每一次读写和每一次起进程都经过闸门判定并记进 trace；进程只能写读写挂载，它改了什么会被观测下来，同样记进 trace。trace 可以通过 hook 落盘，挂载的存储可以随时在磁盘、内存、数据库之间切换。

设计细节见 [docs/design.md](docs/design.md)。

## 现状

| crate | 状态 |
|---|---|
| `wendy-storage` | 完成：磁盘 / 内存 / SQLite 后端，同一套行为测试同时跑三种后端 |
| `wendy-trace` | 完成：事件 + 内存 hook + JSONL 落盘 hook |
| `wendy-io` | 完成：挂载表、闸门、按 actor 和 IPO 操作归属的 trace；`exec` 启动真实进程（Seatbelt 限制写入和网络，FSEvents 观测改动） |
| `wendy-libc` | errno / path / fs / glob / time |
| `wendy-jsc` | 可用：每个插件一个 realm、一个线程；直接运行 TS；node API：`fs`、`path`、`process`、`child_process`、`console`、定时器、`Buffer`、`events`、`util`、`os`；SDK 模块 `wendy` |
| `wendy-core` | 描述层：IPO 操作、插件、工具 |
| `wendy-kernel` | 完成：加载插件、工具调用即 IPO 操作（可嵌套、可唤起）、审批也是 IPO 操作；内置工具 bash / read / edit / create / delete |
| `wendy-cli` | `wendy tools`、`wendy call`、`wendy mcp`、`wendy run` |
| `wendy-sh` | 暂停：自研的对齐 bash 的 sh，目前工具直接用系统 bash；源码保留，不参与构建 |

## 试一下

```bash
cargo build -p wendy-cli
```

默认把当前目录以读写方式挂在它自己的路径上，文件工具和 bash 看到的路径一致：

```bash
./target/debug/wendy tools
```

```bash
./target/debug/wendy call --trace trace.jsonl edit '{"path": "src/main.rs", "old_string": "hi", "new_string": "hello"}'
```

```bash
./target/debug/wendy call bash '{"command": "cargo --version && ls"}'
```

作为 MCP server 给 agent 用（例如 Claude Code）：

```bash
claude mcp add wendy -- /path/to/wendy/target/debug/wendy mcp --trace /tmp/wendy-trace.jsonl
```

自己写插件（类型声明见 [packages/sdk/wendy.d.ts](packages/sdk/wendy.d.ts)）：

```ts
import { definePlugin } from "wendy";
import fs from "node:fs/promises";

export default definePlugin({
  name: "notes",
  tools: {
    count_todos: {
      description: "Count TODO lines in a file",
      schema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] },
      async process({ path }, ctx) {
        const text = await fs.readFile(path, "utf8");               // 走 wendy-io
        const read = await ctx.call("read", { path, limit: 1 });    // 调另一个工具：子操作
        const grep = await ctx.exec(["grep", "-c", "TODO", path]);  // 受限的真实进程
        return { todos: Number(grep.stdout.trim()), lines: read.total_lines, bytes: text.length };
      },
    },
  },
});
```

```bash
./target/debug/wendy call --plugin ./my-plugin count_todos '{"path": "README.md"}'
```

直接运行一个 TS 文件（像 node 一样，定时器跑完才退出）：

```bash
./target/debug/wendy run --mount /work=disk:./project:ro --mount /notes=sqlite:./notes.db ./script.ts -- arg1
```

挂载格式：`AT=disk:PATH[:ro]`、`AT=sqlite:PATH[:ro]`、`AT=memory`，或者 `disk:PATH[:ro]`（挂在它自己的绝对路径上），可以重复多次。
- `--ask`：每次写入和每次起进程前在终端里输入 y / a / n 确认（a 表示这个路径以后都放行）。
- `--deny-write PATH`：拒绝写入该路径下的任何文件，也拒绝在这里起进程。
- `--allow-net`：允许工具启动的进程联网（默认禁止）。
- `--deny-read PATH`：进程不能读这个宿主目录（例如 `~/.ssh`）。
- `--trace FILE`：把所有 trace 事件以 JSON lines 追加到文件，每次运行带一个 `run_id`。

## 测试

```bash
cargo test --workspace
```

目前只支持 macOS（系统 JavaScriptCore、Seatbelt、FSEvents）。
