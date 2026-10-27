# RFC 0003: 审批协调与环境适配器

状态：已实现。当前有效契约以 [SPEC.md](../SPEC.md) 为准；本文件保留方案细节、边界与操作示例。

## 问题与决策

安全建议与环境授权是不同职责。审批协调保留请求绑定，终端或其他环境插件提供用户/服务决定。

风险评估、执行决策和环境交互分三层：

1. `jev` 评估风险，返回概率、建议及 `requires_approval`。高风险、待复核、密钥缺失或审查失败都需要确认，不代表用户已拒绝。
2. `approval` 管理宿主选择的审批策略并协调请求与结果。`risk` 为默认值，只处理已有风险 hook 的请求；`ask` 要求模型工具调用逐次确认；`bypass` 是宿主显式选择的跳过人工确认策略。它没有界面，不调用某个审批平台，也不会伪造用户回答。
3. 环境插件订阅审批事件，实现当前环境的用户确认或外部审批。终端已有 [TUI 插件](0005-tui.md)；GUI、MCP 宿主、企业审批系统可采用各自适配器。TUI 只展示请求和提交决定，不设置模型的工具白名单。

`--jev` 会自动加载协调插件。手动加载时使用 `--plugin ./plugins/approval --plugin ./plugins/jev`。仅加载 Jev 而没有协调插件也会返回 `approval_required`，不会把缺失环境当成批准或最终拒绝。

## 统一协议

现有 bash `approval.request`、`ctx.requestCommandApproval` 和请求格式保持不变。回调收到 `event.data`：

```ts
interface PlyApprovalRequest {
  schema_version: 1;
  id: string;
  tool: "bash";
  command: string;
  cwd: string;
  assessments: unknown[];
  source: { fiber: string; parent: string | null };
  requested_at: number;
}
```

模型调用其他工具时，agent 在启动工具前调用 `ctx.authorizeToolCall`。`ask` 模式发出 `approval.toolRequest`，请求格式为：

```ts
interface PlyToolApprovalRequest {
  schema_version: 1;
  id: string;
  tool: string;
  input: unknown;
  cwd: string;
  source: { fiber: string; parent: string | null };
  requested_at: number;
}
```

请求 ID 由协调插件结合当前 agent fiber 与递增编号生成，同一 agent 并行调用也不会复用。模型不能提供或覆盖请求 ID。source.fiber 是发起调用的 agent；待审工具尚未启动。`input` 是待执行的原始参数，agent 校验返回请求与原参数、cwd、fiber 一致，再执行原调用。

read/create/edit/remove/memory 与模型发起的 agent 委派均走该入口；read 获批前不向模型提供文件正文，预览只使用已经提交的参数。skill 插件提供上下文中的技能元数据，模型读取 SKILL.md 仍走 read 审批，见 [Skills](0006-skills.md)。默认子 agent 的工具集与委派限制继续由 agent 管理，不由审批界面删减。直接由用户启动主 agent 不重复审批。

宿主通过 `KernelConfig.env["PLY_APPROVAL_MODE"]` 选择 risk、ask 或 bypass；CLI 的 `tui` 默认 ask，显式 `ply tui --bypass` 选择 bypass。ask/bypass 模式均经过 bash.beforeExecute，在既有 Jev 检查结束后处理原始命令。bypass 直接写入授权记录，不向适配器发送审批请求。没有命令白名单或 pwd 特例。普通 `--jev` 仍采用 risk 模式。

bypass 不跳过 agent 的工具授权与委派检查、内核子任务配额、挂载限制或普通 before hook 的硬拒绝。Jev 的风险建议仍可记录；需要审批的建议按宿主 bypass 策略继续执行。`approval_bypassed` 的 status 为 approved、reason 为 approval_bypassed、responses 为空，明确区别于人工批准；`approvalStatus().bypassed` 单独累计，requested/approved 仍统计实际审批请求与批准。CLI 不从模型工具参数或任意环境变量开启 bypass，选择发生在宿主启动阶段。

通用入口覆盖 agent 向模型提供的工具调用；宿主直接 `Kernel.call("read", ...)`、CLI `call read` 或可信插件直接访问文件不经过 agent 的通用门禁，仍由相应宿主/插件控制。仅加载 TUI 不改变这些入口。

环境适配器必须等待真实、经过认证的用户或审批服务的回答，并返回：

```ts
interface PlyApprovalResponse {
  request_id: string;
  decision: "approved" | "rejected" | "pending";
  reason?: string;
  reference?: string;
  approver?: string;
}
```

两种事件共用以上响应。适配器应完整展示原始参数和工作目录，不能从模型输出推导“用户已同意”。响应必须匹配本次 request_id；审批回调不能通过修改 payload 改写待执行操作。

多个适配器均明确 approved 才继续；任一 rejected 则终止。没有适配器、返回 pending、响应无效或审批服务失败均保留 pending。普通 before hook 的明确硬拒绝仍生效，不能由另一个审批结果覆盖。

## 待审批与继续执行

- 适配器异步等待最终批准时，同一次 bash 调用保持等待；approved 后只执行原命令一次，rejected 或取消则终止。
- 用户/外部审批等待不消耗 sh 的执行预算。协调期限由宿主 `PLY_APPROVAL_TIMEOUT_MS` 配置，默认 300000 ms，范围 1–86400000。
- `ply tui` 提供真实终端审批；其他入口需另行接入环境适配器。协调层没有订阅者时立即向宿主返回 `status:"approval_required"`、原始请求和原因，不伪造用户确认。
- agent 遇到待审批工具结果会返回同样的待审批状态和 approval_requests，不继续刷模型轮次或生成完成报告。
- 适配器直接返回 pending 表示把处理交给宿主；当前没有跨进程自动审批恢复或旧命令自动重放。要在同一次调用继续，应在回调中 await 最终批准。若之后重新发起调用，适配器必须重新核对请求与既有审批的对应关系。

尚未执行的 bash 结果保留原输出字段，同时增加：

```json
{
  "status": "approval_required",
  "exit_code": null,
  "signal": null,
  "timed_out": false,
  "stdout": "",
  "stderr": "",
  "approval": {
    "status": "pending",
    "request": { "id": "...", "tool": "bash", "command": "...", "cwd": "..." },
    "responses": [],
    "reason": "approval_provider_missing"
  }
}
```

这不是命令执行成功，也不是最终拒绝。宿主应显示或路由该请求。已经得到批准的执行结果包含 approval 证据；没有审批需求的原有输出格式不变。

## 适配器模板

[examples/plugins/approval-adapter/main.ts](../examples/plugins/approval-adapter/main.ts) 可加载但永远返回 pending，只展示协议，不是实际审批环境：

```ts
import plugins from "ply:plugins";
import * as events from "ply:events";
export async function onLoad() {
  await plugins.approval.loaded;
  const pending = async (event: events.Event<PlyApprovalRequest | PlyToolApprovalRequest>): Promise<PlyApprovalResponse> => {
    // 在这里接入该环境的确认 UI 或审批 API，并 await 真实回答。
    return { request_id: event.data.id, decision: "pending", reason: "environment not connected" };
  };
  await events.on("approval.request", pending);
  await events.on("approval.toolRequest", pending);
}
```

不要将固定 approved 的测试桩作为生产适配器。审批插件/适配器都不注册模型工具；没有可由模型调用的“自我批准”工具或 approved 参数。

运行轨迹记录 approval_requested、approval_result、approval_bypassed、tool_approved、command_approval_required、command_approved 和 agent_approval_required；每个适配器回调仍有自己的子 fiber 与取消生命周期。`ctx.approvalStatus()` 汇总协调结果、bypassed 次数、模式与期限，不代表存在可用环境。
