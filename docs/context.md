# 上下文

context 插件独占模型上下文的构建。其他插件通过 `context.set` 提供内容（见 [写插件](plugins.md#向上下文提供内容)），顺序固定为 Rules → Project → Extensions → State → Reference。Rules、Project、Extensions 构成冻结前缀；State、Reference 的更新追加到完整工具批次之后，Reference provider 在每个完整工具批次后刷新，相同内容不产生新版本。设计见 [RFC 0007](rfcs/0007-context.md)。

## 预算

默认总窗口为 **131072 token**，其中预留 **16384 token** 给输出，可用输入预算为 **114688 token**。

- 配置的窗口还受模型的真实窗口约束；输出预留同时是每次任务回复的 `maxTokens`，不会超过模型的最大输出。
- 窗口约束单次模型请求，不限制累计 token 或轮数；每个主、子 agent 会话独立应用同一配置。
- 每次模型调用前检查预算：第一次按 UTF-8 字节数 / 2 加消息与工具定义开销保守估算；收到有效 usage 后，用该次实际输入 + 输出作为历史前缀的锚点，只估算之后新增的内容。**这是保护性估算，不是供应商 tokenizer 的精确计数。**

## 配置

```bash
./target/debug/ply call --context-config examples/context.json --record fibers.jsonl agent '{"prompt":"完整评审这个项目"}'

./target/debug/ply call --max-context-tokens 262144 --context-reserve-tokens 16384 --context-compact-trigger 0.8 --context-compact-target 0.5 agent '{"prompt":"继续长任务"}'
```

[examples/context.json](../examples/context.json) 的字段：

| 配置 | 默认值 | 含义 |
|---|---:|---|
| `max_context_tokens` | 131072 | 输入与预留输出合计的窗口 |
| `reserve_output_tokens` | 16384 | 输出预留与任务请求的输出上限 |
| `compaction.enabled` | true | 是否自动摘要压缩；关闭后仍执行输入预算检查 |
| `compaction.trigger_ratio` | 0.8 | 输入估算达到可用输入预算的这一比例时触发 |
| `compaction.target_ratio` | 0.5 | 压缩希望达到的输入预算比例，必须小于触发比例 |
| `compaction.keep_recent_turns` | 4 | 原样保留的最近完整交互数，一个工具批次及其全部结果算一组 |
| `compaction.summary_max_tokens` | 2048 | 摘要请求的输出上限，不得大于输出预留 |
| `compaction.max_retries` | 1 | 每个摘要失败后的额外重试次数，范围 0–10 |
| `compaction.retry_delay_ms` | 500 | 重试间隔，范围 0–60000 ms |
| `compaction.failure_cooldown_turns` | 2 | 预算允许延后时，暂停软压缩的轮数，范围 0–100 |

显式 CLI 参数覆盖 JSON 文件，未配置项使用默认值；`--context-compaction off` 关闭自动压缩。未知字段、非正整数、无效比例和矛盾预算会报错；不会自动读取当前目录中的配置文件。Rust 宿主可以把同样的 JSON 写入 `KernelConfig.env["PLY_CONTEXT_CONFIG"]`。

## 压缩

达到阈值后，用当前模型把较早的完整交互合并成摘要，保留原始任务、已有摘要中的有效事实以及最近的交互。

- 摘要请求不提供工具，不额外开启思考，并限制摘要输出。过长的历史分成多个不拆工具调用的摘要批次，每批都检查输入预算。
- 目标可能因必须保留的原始任务或最近交互而无法完全达到；仍在预算内且预计压缩无收益，或完整交互放不进摘要请求时，延后并记录原因。
- 摘要生成、内容校验和上下文版本核对都成功后才替换旧历史；空摘要、截断摘要、工具调用和失败响应都保留旧历史并有限重试。
- 重试耗尽但仍在硬预算内时，延后软压缩并继续任务；冷却期内超过硬预算仍必须重新压缩。超过输入硬预算则明确报错，不发送超预算的请求。

压缩事件是 `agent.context_compaction`，用 `status` 区分 `completed`、`failed` 和 `deferred`，记录重试、前后估算 token、替换消息数、目标是否达到、冷却原因及耗时。压缩改变历史前缀，可能降低之后的缓存复用率，缓存命中率仍以真实上报值累计。

## 会话

会话 id 就是使用该会话的能力，context 不按调用方判定归属。子 agent 以 `inherit` 继承父会话的只读快照，不能更新父会话。模型工具和顺序在会话打开时冻结。
