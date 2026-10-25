# TUI 插件与终端权限审批

`wendy tui` 启动内置 `tui` 插件在 `wendy:commands` 注册的入口，在当前前台终端与 agent 交互、查看结果和用量，并批准或拒绝模型发起的工具调用。TUI 通过 `import plugins from "wendy:plugins"` 获取目录，只调用 `plugins.agent({ prompt })`，不直接调用其他业务工具，也不设置 agent 的工具白名单、模型轮数或委派参数。界面逻辑是 TS 插件；原生 `wendy:terminal` 提供 `/dev/tty` 输入、屏幕绘制和终端恢复，不启动外部 Node。

```bash
cargo build -p wendy-cli --locked --offline
./target/debug/wendy tui --record /tmp/wendy-tui.jsonl

# 可选：直接提供第一个任务
./target/debug/wendy tui "只读分析当前项目，先列出目录"

# 叠加 Jev 风险评估，自动审查读取宿主 TYPESAFE_API_KEY
./target/debug/wendy tui --jev

# 切换模型，读取宿主 MIMO_API_KEY
./target/debug/wendy tui --model mimo-v2.6-pro
```

普通任务使用宿主 `DEEPSEEK_API_KEY`，默认 DeepSeek。`--mount`、`--context-*`、`--agent-max-children`、`--agent-report-section` 和 `--plugin` 等通用选项也可使用。默认挂载当前工作目录。只读磁盘可以显式使用 `--mount disk:PATH:ro`。

## 输入与结果

| 输入 | 行为 |
|---|---|
| 普通文字 + Enter | 启动一个 agent 任务，结束后显示报告与自身子树的 token 用量、缓存命中率 |
| `/help` | 查看快捷键与命令 |
| `/clear` | 清除界面保留的任务文本 |
| `/quit` 或 `/exit` | 空闲时退出 |
| ↑↓、PageUp、PageDown、Home、End | 翻阅结果 |
| Ctrl-C / Ctrl-D | 退出；任务运行中会取消当前任务及其等待中的子 fiber |
| Ctrl-U | 清空当前输入 |

每个普通任务是独立的 agent 对话，不把上个任务的报告自动放进新模型上下文。任务完成后显示报告，当前没有模型文本逐 token 流式展示。单次输入上限 65536 字符，超限拒绝提交；界面保留最近 200 条文本记录，需要完整轨迹时使用 `--record`。

用量小计来自已返回的 agent `total_usage`，不重复累加各级子 agent。启用 Jev 后，其检查用量也按原有子树口径汇总。缺失缓存分项显示“未知”，不填 0。失败任务用量以 JSONL 中的 model_call / agent_usage 为准，界面标记会话小计不完整。

## 权限范围

工具目录与执行审批分别由 agent 和 [approval 插件](approval.md) 管理：

- 默认主 agent 拿到 `agent/bash/create/edit/read/remove` 六个工具；默认子 agent 由已有委派限制移除 `agent`，保留另外五个。显式收窄工具集仍属于 agent 的能力，TUI 不额外传 `allowed_tools`。
- CLI 宿主为 TUI 选择 `ask` 审批策略：模型调用 read/create/edit/remove、其他已注册工具以及 agent 委派，均在 agent 的执行层先审批；bash 在自身的执行前检查和 Jev 评估后审批一次。没有 pwd 特例。
- read 获准前不会启动文件工具或读取内容；create/edit 展示已提交的内容、替换参数；remove 展示路径与递归参数。预览不会提前读取目标文件。批准绑定原始参数，不能通过响应改写调用。
- 默认允许一代子 agent，每个 agent 最多累计创建两个直属子 agent；子级不能继续委派。`--agent-max-children` 可调整数量，模型轮数仍不设上限。这些限制由 agent 自己的默认策略处理。
- 审批显示原始调用参数或命令、cwd、请求 ID、fiber 和已知风险结果。长请求可滚动，完整请求到达末页后才启用批准；模型输出和粘贴内容不能当作批准输入。
- `y` 只批准当前操作一次；`n` / Enter / Esc 拒绝。用户输入任务启动主 agent，无需重复批准；模型进一步委派子 agent 属于需审批的工具调用。
- 并发请求逐个显示和关联，同一 agent 的请求有独立 ID；上一请求剩余按键不会批准下一请求。取消、超时后移除过期审批，不启动待审工具。
- 审批等待默认五分钟，由 `WENDY_APPROVAL_TIMEOUT_MS` 配置；不消耗 sh 的执行期限。超时仍返回待审批状态，没有跨进程自动恢复或旧命令自动重放。

当前终端用户是批准主体，不另做账户登录。TUI 必须能打开真实的、属于前台进程组的 `/dev/tty`；管道 stdin、MCP 消息或模型输出都不作为批准来源。没有终端则明确失败，不降级为自动批准。

## 插件接入

`tui` 是 opt-in 内置插件。`wendy tui` 自动加载 `approval` 和 `tui`，宿主在 `KernelConfig.env` 中选择 `WENDY_APPROVAL_MODE=ask`。仅加载 TUI 插件不会打开界面或选择审批策略。TUI 只处理自己 fiber 子树的审批，不代替其他环境批准。

`onLoad` 订阅 `approval.request` 和 `approval.toolRequest`，并通过 `commands.register({ name:"tui", handler:onHandle })` 注册入口；回调复用已有界面实现。TUI 不订阅工具的 beforeExecute 来定义策略，也不调用 `ctx.registerTool`。Rust 嵌入时由宿主选择审批模式、加载插件、等待 ready，再 `Kernel::run_command("tui", input)`；在 MCP 等环境应使用相应的宿主适配器。注册接口见 [命令文档](commands.md)。

`wendy:terminal` 的 session 由打开它的 fiber 所有，原生层在成功、失败或取消时回收。正常路径也在 finally 关闭；即使 JS 清理失败，宿主仍会恢复 termios、光标、粘贴模式及原屏幕。UI 不解释命令/模型文本中的 ANSI、OSC、双向控制字符；小于 40×10 的终端不能批准。

真实 PTY 验收见 [crates/wendy-cli/tests/tui.rs](../crates/wendy-cli/tests/tui.rs)。测试只使用本地 fixture 与临时目录，不调用供应商，也不授权任何测试目录之外的命令。
