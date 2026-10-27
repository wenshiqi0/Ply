# agent

agent 插件用语言模型完成一个任务：每轮让 context 准备上下文，调用模型，执行模型请求的工具，直到模型给出通过校验的最终报告。每次工具调用都是 agent 的子调用，记录里能看到完整过程。上下文的构建和压缩见 [上下文](context.md)，模型接入见 [模型接入](models.md)。

## 委派子 agent

`agent` 本身也是模型可见的工具：模型调用 `agent({ prompt, allowed_tools?, max_delegation_depth?, max_children?, report_sections?, max_turns? })` 时运行一个子 agent，父 agent 等它完成，结果作为工具输出返回；同一轮发起的多个子 agent 并发运行。

每个子 agent 新建独立对话，只接收传入的 `prompt` 和父会话的只读上下文快照，父 agent 要在 prompt 里提供任务背景和约束。父子 agent 共用挂载存储、工作目录和模型。父 agent 通过一次性的委派凭据把自己的限制和用量链交给子 agent，子 agent 不需要知道是谁调用了它。

| 参数 | 含义 |
|---|---|
| `allowed_tools` | 工具白名单；省略时取已注册工具的快照。子 agent 只能收窄父级授权，默认也没有 memory 工具；请求父级没有的工具会报错 |
| `max_delegation_depth` | 还能创建多少代子任务：主 agent 默认 1，子任务继承后减 1；为 0 时移除 `agent` 工具并拒绝继续委派。更深递归须由主调用显式授予，还受内核嵌套深度限制（默认 16） |
| `max_children` | 每个 agent 累计创建直属子 agent 的硬上限，主调用默认 **2**；不是并发数，也不限制普通工具 |
| `report_sections` | 最终报告必须包含的 Markdown 标题，见下文 |
| `max_turns` | 默认不限制模型轮数；只有显式传入正整数时才限制对应 agent 的轮数 |

`max_children` 由 agent 插件在审批之后、发起子调用之前同步检查并计数，超额请求成为工具错误，不创建子调用。失败、完成、取消以及替换和恢复都不退还名额。返回值的 `delegation` 给出 `limit`、`started`、`remaining`。CLI 的 `--agent-max-children COUNT` 设置主调用默认值，调用 JSON 中的 `max_children` 优先。

只读、仅允许一层子任务的例子（工具白名单仍需配合可信插件使用，不等于 OS 沙箱）：

```bash
./target/debug/ply call agent '{"prompt":"启动两个子 agent 只读评审项目，再汇总", "allowed_tools":["read","agent"], "max_delegation_depth":1, "max_children":2, "report_sections":["架构判断","发现","验证边界"]}'
```

用提示词限制工作范围时，仍应同时设置这些结构化权限。

## 最终报告

- 最终报告必须正常停止、非空、代码围栏闭合，正文不得残留工具控制标记；内部使用结束标记 `[[PLY_REPORT_COMPLETE]]`，返回前删除。
- `report_sections` 要求指定的 Markdown 标题及非空内容；标题要求会传给子 agent，子级和恢复调用只能增加要求。CLI 可重复传 `--agent-report-section HEADING`。
- 校验失败时发出 `agent.report`（`status: "rejected"`），在原 agent 内重新生成完整报告，不创建替代 agent，不把有问题的原文放回模型历史。通过后发出 `status: "validated"`，返回值带 `report_validation`。
- 这只校验格式和完成状态，不证明结论正确。带半截工具调用的 `length` 响应立即失败并保留可恢复状态，不执行部分参数。

## 用量统计

agent 的返回结果带两份用量汇总：`usage` 只统计当前 agent 自己的模型调用，`total_usage` 还包括它委派的子 agent。子 agent 的用量逐轮计入祖先，已报告的失败调用也会计入；父 agent 不会重复加上子 agent 返回的汇总。Jev 等旁路插件的模型用量只在它们自己的事件里，不计入 `total_usage`。

| usage 字段 | 含义 |
|---|---|
| `calls` / `reported_calls` / `missing_calls` | 模型调用次数 / 有有效 usage 的次数 / 缺失 usage 的次数 |
| `input_tokens` | 全部输入 token，包含未命中、缓存读取和缓存写入 |
| `uncached_input_tokens` | 普通输入 token，不含缓存读取/写入，对应 pi-ai 的 `usage.input`；未报告分项时为 null |
| `output_tokens` / `reasoning_tokens` | 输出 token / 其中的推理 token；推理量未报告时为 null |
| `cache_read_tokens` / `cache_write_tokens` | 缓存读取 / 缓存写入 token；供应商未报告分项时为 null |
| `total_tokens` | 输入 + 输出，推理 token 不重复相加 |
| `cache_hit_rate` | 累计缓存读取 token / 累计全部输入 token，按 token 加权；缓存分项未知时为 null |
| `complete` | 是否每次模型调用都有有效 usage；为 false 时是已报告部分的小计 |

没有 usage 或 SDK 的全零占位 usage 都记为缺失，不当作实测 0 用量；单次调用内部的 SDK 重试不另算一轮。

每次付费模型调用前发出 `agent.model_call_started` 事件，拿到响应或异常后发出对应的 `agent.model_call`，用 `call_index` 关联；强制取消可能只留下 started，表示用量未知而不是 0。`agent.model_call` 的 `purpose` 区分 `task`、`compaction` 与 Jev 的 `safety`，并记录轮次、模型、响应 ID、停止原因、耗时、usage 或错误。`turn` 是循环轮次，`usage.calls` 包含摘要调用，两者可能不同。

用 `--record` 保存后，插件事件写成 `event: "plugin"` 行（见 [命令行](cli.md#记录文件)）。统计整棵调用树时，按 spawn 的父子关系汇总 `agent.model_call` 与 `jev.model_call`，或读取根 agent 的 `total_usage`（不含 Jev），不要再叠加各级 `total_usage`。

## 失败恢复

无法继续时，agent 在宿主私有目录 `$HOME/.ply/checkpoints/` 保存原任务、已完成交互、摘要、模型配置、权限、已消耗的直属子 agent 名额、报告校验状态和下一轮位置；错误信息和 `agent.checkpoint` 事件给出 `resume_id`。

```bash
./target/debug/ply call agent '{"resume_id":"错误信息中的恢复 ID"}'
```

- `PLY_CHECKPOINT_DIR` 可指定其他绝对路径；目录权限 0700、文件 0600，写入采用临时文件 + fsync + 原子替换。快照可能含任务和文件内容，不包含宿主 API key。
- agent 恢复格式为 4，其中的上下文格式为 2；旧格式明确拒绝恢复，需要发起新任务。
- 恢复时须使用相同的工作目录，会重算窗口预算、重新检查工具权限，不能扩大原授权；已花费的子 agent 名额和报告校验状态保留。
- 恢复记录有跨进程互斥锁，成功后删除快照。执行工具前标记 `tools_pending`，工具结果全部保存后才回到 `ready`，工具执行到一半的状态不会自动重放。
- 原始模型轮数延续，`max_turns` 限制的是本次调用新增的轮数；历史累计用量以 `previous_usage` / `previous_total_usage` 单列。
