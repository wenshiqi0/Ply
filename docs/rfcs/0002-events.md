# RFC 0002: 插件事件和 bash 执行前 hook

状态：已实现。当前有效契约以 [SPEC.md](../../SPEC.md) 为准；本文件保留方案细节、边界与操作示例。

## 问题与决策

插件需要在 onLoad 声明事件，bash 工具需要在脚本产生副作用前异步等待订阅者；选择内核归属的跨 realm 事件。

模型可见的 `bash` 工具通过原生 `ply:sh` 调用内部解释器。CLI/MCP 直接调用这个插件也走相同后端。`command`、`cwd`、`timeout_ms` 及输出字段保留，其他插件直接使用的 `node:child_process` 不受该工具后端选择影响。

## 在 onLoad 声明自定义事件

```ts
import * as events from "ply:events";
let changed: string;

export async function onLoad() {
  changed = await events.define("changed"); // 返回 example.changed，example 是插件目录名
}

export async function onHandle(input: { value: string }) {
  return await events.emit(changed, input);
}
```

定义和订阅必须在 onLoad 中 await 完成。事件名由内核附上插件命名空间，只能由所属插件发布；onLoad 阶段只注册，不发布。`events.list()` 可列出名称和订阅数。订阅方先等待 `plugins.<provider>.loaded`，再调用 `events.on`。原有 onLoad 返回 data/methods 挂载到 ctx 的接口保留。

## 订阅 bash.beforeExecute

完整可加载示例在 [examples/plugins/bash-guard/main.ts](../../examples/plugins/bash-guard/main.ts)：

```ts
import plugins from "ply:plugins";
import * as events from "ply:events";

type Request = { command: string; cwd: string; timeout_ms: number };
export async function onLoad() {
  await plugins.bash.loaded;
  await events.on<Request>("bash.beforeExecute", async event => {
    await fiber.record({ type: "before_command", source: event.source, command: event.data.command });
    if (event.data.command.includes("__deny_demo__")) {
      return { allow: false, reason: "demo hook rejected the request" };
    }
    return { allow: true };
  });
}
```

`__deny_demo__` 只用于演示 hook，不是完整的 shell 权限策略。将自己的校验、异步询问或审计逻辑放进监听器即可。回调入参包含内核生成的 `id`、`name`、`source:{plugin,fiber}` 和发布方的 `data`。

监听器按订阅顺序串行运行，并异步等待。正常返回值收集成数组；`false` 或 `{allow:false,reason}` 由 bash 在分发结束后判为拒绝。抛异常会立即使分发失败。超时和取消会取消在途监听器，bash 不进入解释器。

该 hook 覆盖**整个工具调用**，并且在任何脚本解析执行、命令替换或重定向之前完成；第一版不为脚本内部每条命令触发。`timeout_ms` 覆盖检查 hook 与 sh 阶段，sh 只获得剩余时间；环境审批等待单独计时。带 requires_approval:true 的结果经 [审批协调](0003-approval.md) 处理，批准后执行，缺少环境时返回待审批状态。hook 阶段超时是工具错误；sh 阶段超时返回 `timed_out:true` 和已有输出。内部解释器没有可报告的进程信号，`signal` 为 null。

## 加载和验证

在仓库目录执行：

```bash
cargo build -p ply-cli --locked
./target/debug/ply call --plugin ./examples/plugins/bash-guard --record /tmp/ply-hook.jsonl bash '{"command":"echo hello"}'
./target/debug/ply call --plugin ./examples/plugins/bash-guard bash '{"command":"echo __deny_demo__ > blocked-demo.txt"}'
```

第一条工具调用会放行；第二条报告 demo 拒绝，不创建 `blocked-demo.txt`。该示例插件不注册模型工具，模型仍只看到原来的 bash 工具。

事件回调留在订阅插件的 realm 中，每次运行都有自己的子 fiber：`fiber.parent` 指向发布 fiber，`fiber.event` 为事件名，取消及错误按该调用隔离。回调完成后会回收其未等待的后台任务；需要完成的工作必须 await。事件 payload 为 JSON 副本，修改自己的副本不会改写 bash 的原始请求。

JSONL 中的 `event:"plugin"` 记录发布及真实来源，回调另有配对的 Spawn/Exit。回调不是插件 onHandle，也不作为 agent 工具调用消耗委派名额。`events.on` 返回订阅编号，所属插件可以用 `events.off(id)` 取消订阅。加载失败会回收该插件的事件定义和订阅。

内部 sh 的语法、命令集和执行边界见 [ply-sh README](../../crates/ply-sh/README.md)。当前仅执行内置命令和注册的 utility，未知宿主程序不会被自动透传或交给系统 shell。
