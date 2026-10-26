# CLI 命令注册与插件导入

`ply:commands` 注册 CLI 顶层入口。TUI 插件在 onLoad 中注册 `tui`，CLI 先加载插件并等待 ready，再由命令注册表执行回调。因此 `ply tui` 直接打开交互面板，回调仍在 TUI 插件的 JSC realm 和独立 fiber 中运行。

## 注册与启动

```ts
import * as commands from "ply:commands";

export async function onLoad() {
  await commands.register({
    name: "hello",
    description: "向指定名字问好",
    handler: (input: { name?: string }) => ({ message: `你好，${input?.name ?? "Ply"}` }),
  });
}
```

可运行的例子在 [examples/plugins/hello-command/main.ts](../examples/plugins/hello-command/main.ts)：

```bash
# 不需要模型密钥，不启动模型调用
./target/debug/ply hello --no-default-plugins --plugin ./examples/plugins/hello-command '{"name":"Ply"}'
./target/debug/ply commands --no-default-plugins --plugin ./examples/plugins/hello-command

# 内置命令：直接打开终端交互面板
./target/debug/ply tui
./target/debug/ply tui --model mimo-v2.6-pro

# 查看已加载的命令（默认包含内置 tui）
./target/debug/ply commands
```

自定义命令接收一个 JSON 入参，省略时为 `{}`，结果输出为 JSON；console 输出走 stderr，避免混入 JSON 结果。`--plugin`、`--mount`、`--model`、`--record` 等配置与 `call` 共用解析规则。`tui` 保留可选的普通文字首条消息参数。

注册名必须匹配 `[a-zA-Z0-9_-]{1,64}`，全局唯一。CLI 内置的 tools、commands、call、mcp、run、help 为保留名；重复注册明确失败，不覆盖已有命令。description 可选，最多 4096 UTF-8 字节。回调自行校验输入，返回值必须可序列化为 JSON。

## 模块 API

```ts
await commands.register({ name, description, handler }); // 只在插件 onLoad 中注册
const entries = await commands.list();                  // [{ name, description, plugin }]
const result = await commands.run("hello", { name: "Ply" });
```

注册归属由原生 realm 身份决定，脚本不能伪装同名插件注册。onLoad 失败会撤销该插件的命令，其他插件的注册保持有效。onLoad 期间允许注册、列举，但不能执行命令，避免加载阶段相互等待。

每次执行的 `Spawn.fiber.event` 为 `command:<name>`，父子关系与取消传播继续由内核维护。回调成功结束、失败或取消都会回收其未完成的定时器、IO、子调用；需要完成的工作应 await。命令回调不消耗插件 onHandle 的子调用配额，其内部真正启动的 onHandle 调用仍受原配额约束。

命令不自动成为模型工具或 MCP 工具；这些仍由插件的 onHandle、context 注册与 agent 权限管理。命令注册与插件调用是两个不同的接口。

Rust 宿主对应 `Kernel::commands()` 和 `Kernel::run_command(name, input)`。

## 显式插件导入

旧的全局 `tools` 已移除，不提供兼容别名。插件调用统一写为：

```ts
import plugins, { context, read } from "ply:plugins";

await context.loaded;
const lazy = read({ path: "README.md" });
const result = await lazy;
const names = Object.keys(plugins);
```

具名导出与默认动态目录引用同一稳定插件句柄，可以在插件尚未加载时导入，再等待 loaded。onLoad 返回的接口通过对应句柄访问，例如 context.set；默认动态目录仍反映后续加载的插件。`description`、`parameters`、`wrapsInput` 和 `.loaded` 保留原语义。调用句柄仍在被 await 时创建子 fiber，多次 await 同一个句柄只执行一次。

模块导入不会安装 `globalThis.plugins`。`ctx`、`fiber` 继续保留；模型协议中的 `context.tools`、SDK 的 tools 字段以及 CLI 的 `tools` 子命令均保留原名称，它们表示模型工具定义。

SDK 类型见 [packages/sdk/globals.d.ts](../packages/sdk/globals.d.ts)，验收见 [命令与 fiber 测试](../crates/ply-kernel/tests/commands.rs)及 [CLI 测试](../crates/ply-cli/tests/entrypoints.rs)。
