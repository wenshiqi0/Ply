# RFC 0007: Context

状态：已实现。当前有效契约以 [SPEC.md](../SPEC.md) 为准；本文件保留方案细节、边界与操作示例。

## 问题与决策

插件自行分配 order 会产生耦合并破坏缓存。选择语义类型与中央排序，冻结固定前缀，将动态更新追加到完整交互之后。

上下文由 context 插件构建和保存。其他插件提供内容，agent 提供任务、模型限制和运行事实；agent 不拼接 system prompt，也不理解 skill 的目录、渲染或恢复格式。

## 插件访问

```ts
import { context } from "ply:plugins";

export async function onLoad() {
  await context.loaded;
  await context.set({
    type: context.Type.Extensions,
    key: "capabilities",
    content: "Available capabilities and how to use them.",
  });
}
```

具名导出与默认目录中的 `plugins.context` 是同一个稳定句柄。它可以在 context 加载前被导入；`loaded` 等待 onLoad 发布，之后接口按所属插件解析，方法在 context 的 realm 中执行。所有插件均采用此规则；默认目录仍支持动态的 `plugins[name](input)`，原有 ctx 平铺接口保留。

## 类型、顺序和更新

插件选择用途，不提供 order、mode 或 source。这些额外字段会被拒绝。

| `context.Type` | 用途 | 处理策略 |
|---|---|---|
| Rules | 行为规则 | 固定，进入 system 前缀 |
| Project | 项目背景、工作目录 | 固定，进入 system 前缀 |
| Extensions | skills 等扩展能力说明 | 固定，进入 system 前缀 |
| State | 工作区、任务及运行状态 | 内容变化才排队追加更新 |
| Reference | 补充参考材料 | 按 key 保存有效版本，变化才追加 |

顺序固定为 Rules → Project → Extensions → State → Reference。同一类型内先处理 context 的内部内容，再按实际来源插件名和 key 排序；异步加载完成顺序不参与排序。固定前缀不包含 fiber ID、调用时间和剩余次数。工具定义按名称和稳定 JSON 顺序冻结到每个会话，新加载的工具只影响新会话。

`set` 参数是 `{type,key,content,conversation?}`。key 在实际来源插件的命名空间下，来源由内核提供，插件不能冒充其他来源。同来源/key 必须保持原类型；相同内容是 no-op，不增加版本和消息。`content:null` 删除当前有效值，并保留版本记录。

- **onLoad 中调用**配置模板。根会话会等待贡献者加载成功再复制模板，失败的 onLoad 不会让其内容进入模型。
- State/Reference 可在 onLoad 使用 `{type,key,content:null,provider:"methodName"}` 声明动态数据来源。方法必须由该插件 onLoad 返回，返回 string 或 null；context 在新根会话与根恢复时获取快照，子会话继承父快照。`context.refresh({key,conversation?})` 只刷新调用插件自己声明的来源和所属会话，按相同顺序排队更新，不修改固定前缀。来源失败产生不可用说明，不中断模型任务。
- **运行中的调用**绑定最近的所属会话，只允许修改 State/Reference。固定类型建立后不可原地重写。
- 插件工具和事件回调在 agent 的 fiber 子树内时，更新所属 agent 的会话。显式 conversation 也必须匹配自己的会话；宿主可通过 Kernel.call_plugin_method("context", ...) 操作指定会话。
- 子会话复制父会话的内容块，再按自己的权限和运行事实构造内部规则及状态。副本互相独立。

运行阶段的示例，适用于已有 agent 会话内的工具或事件回调：

```ts
await context.set({
  type: context.Type.State,
  key: "workspace",
  content: JSON.stringify({ branch: "main", dirty: true }),
});
```

## 固定前缀

context 保存“最新内容块”和“已发送消息历史”两份状态。State/Reference 变化时更新最新值、合并同 key 的待发变更；在下一次 prepareContext 的安全边界追加 context update 消息，不把新值插回已发送的前部。

```text
第 1 次请求：固定前缀 → 历史 → 更新 v1
第 2 次请求：固定前缀 → 历史 → 更新 v1 → 新交互 → 更新 v2
```

更新不会插入工具调用与工具结果之间。正在执行模型请求时收到的状态更新先排队，不改变该请求的 revision；普通交互与完整工具结果提交后再应用。更新消息使用现有 pi-ai 支持的 user 内容，状态标识放在自有元数据中，不依赖新版 SDK 的中途 system section 支持。

prepareContext 的 budget 带 `context_epoch` 和 `fixed_prefix_fingerprint`；model_call 的已有预算记录会带上它们。fingerprint 是固定 system 和 tools 的轻量诊断值，用于检查意外变化，不作为授权校验、缓存 key 或缓存命中保证。实际缓存命中仍以供应商 usage 为准。

`context.inspect(id)` 可查看固定块顺序、工具顺序和待发更新；`context.blocks(id?)` 返回模板或所属会话的内容块。

## 构建与压缩

agent 调用 `context.openTask({prompt,model,allowed_tools,facts})`。context 生成基础规则、项目上下文与首条用户消息；剩余委派名额作为 State 更新。报告修复提示和工具结果消息也由 context 统一生成。

每轮 `prepareContext` 负责应用待发状态、估算 token、生成模型请求或压缩计划。agent 执行计划中的模型调用，提交模型输出和 usage，不重新组装内容。

压缩按完整交互切分，将最新 State/Reference 折叠为新的状态快照，保留原任务和最近交互。旧状态更新从保留历史中折叠掉，避免旧版本覆盖新快照；epoch 增加、usage anchor 清空，固定前缀仍保留。这是明确的历史缓存重建边界。

## 恢复与限制

agent 恢复记录升级为 **format 4**；context 自己的会话快照为 **format 2**。快照包含固定前缀、内容块和版本、待发更新、工具定义、epoch、当前状态快照、消息及摘要。旧格式明确拒绝，不提供迁移分支。

恢复不重新扫描 skill 或用当前模板覆盖旧会话。只有明确收窄权限、改变规则或当前工具 schema 改变时，context 才重新构造相应固定前缀并增加 epoch。恢复后的执行权限仍由 agent 和内核约束。

最多 128 个内容块，每块正文最多 256 KiB，块数据总计最多 1 MiB；还受配置的上下文 token 预算保护。读取、写入和审批的既有边界保持不变。

实现见 [context 插件](../plugins/context/main.ts)、[插件句柄](../crates/ply-jsc/src/prelude.js)、[原生调用来源](../crates/ply-kernel/src/lib.rs)。回归用例源头见 [context_blocks.rs](../crates/ply-kernel/tests/context_blocks.rs)。
