# wendy

一个 agent harness。Rust 是主核心，插件用 TS 写：每个插件就是一个函数 `handle(input)`，每次执行是内核调度的一个 fiber。插件之间不互相调用，只互相等待：`await tools.read({ path })` 的那一刻就是调用。

wendy 记录的是 fiber：每个 fiber 的输入、输出（或错误），以及谁等待了谁。插件只能看到挂载的存储，挂载的存储可以在磁盘、内存、数据库之间切换。

设计细节见 [docs/design.md](docs/design.md)。

## 写一个插件

一个目录就是一个插件，目录名就是插件名。`main.ts` 导出一个 `handle`：

```ts
// todos/main.ts

/** Count TODO lines in a file. */
export async function handle(input: { path: string }): Promise<{ todos: number; lines: number }> {
  const [file, grep] = await Promise.all([
    tools.read({ path: input.path }),                                   // 等待另一个插件：一个子 fiber
    tools.bash({ command: `grep -c TODO ${JSON.stringify(input.path)}` }),
  ]);
  return { todos: Number(grep.stdout.trim()), lines: file.total_lines };
}
```

- `handle` 上的 JSDoc 是插件的描述，`handle` 的参数类型和返回类型就是它的输入和输出（wendy 加载时从签名里读出来）。
- 参数校验由插件自己做；权限这类控制也放在插件里。
- 同一个插件的多次执行共享模块里的闭包，在 `await` 处交替执行。
- 可以用 node 的模块（`fs`、`path`、`child_process` 等），类型声明见 [packages/sdk/globals.d.ts](packages/sdk/globals.d.ts)。

默认带五个插件，写法和上面一样：[bash](plugins/bash/main.ts)、[read](plugins/read/main.ts)、[edit](plugins/edit/main.ts)、[create](plugins/create/main.ts)、[delete](plugins/delete/main.ts)。

## 试一下

```bash
cargo build -p wendy-cli
```

默认把当前目录以读写方式挂在它自己的路径上：

```bash
./target/debug/wendy tools
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
- `--no-default-plugins`：不加载默认的五个插件。
- `--record FILE`：把每个 fiber 事件（spawn / exit）以 JSON lines 追加到文件。

## crate

| crate | 内容 |
|---|---|
| `wendy-storage` | 磁盘 / 内存 / SQLite 后端，同一套行为测试同时跑三种 |
| `wendy-io` | 挂载表（mount / unmount / swap，只有挂载的路径存在）；在磁盘挂载上启动真实进程 |
| `wendy-libc` | errno / path / fs / glob / time |
| `wendy-jsc` | 内嵌 JavaScriptCore，直接运行 TS；每个插件一个 realm、一个线程；node API 和全局 `tools` |
| `wendy-core` | 描述层：插件、输入输出的类型、fiber、fiber 事件和 hook |
| `wendy-kernel` | 加载插件、读出签名、调度 fiber、发出 fiber 事件、默认插件 |
| `wendy-cli` | `wendy tools`、`wendy call`、`wendy mcp`、`wendy run` |
| `wendy-sh` | 暂停：自研的对齐 bash 的 sh，源码保留，不参与构建 |

## 测试

```bash
cargo test --workspace
```

目前只支持 macOS（系统 JavaScriptCore）。
