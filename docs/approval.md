# 审批与安全

审批是插件之间的协议：approval 插件决定策略，环境适配器（TUI 或其他环境）给出人工或服务的决定，模型本身没有自我批准的入口。协议见 [RFC 0003](rfcs/0003-approval.md)。

## 审批流程

- bash 的风险复核（来自 `bash.beforeExecute` 监听器）和其他模型工具的调用前审批都进入 approval 协调。
- approval 发出 `approval.request` / `approval.toolRequest` 事件，由订阅它们的环境适配器回答；请求的 id 由 approval 分配，只用于展示和日志。请求方按提交的内容（command 与 cwd，或 tool、cwd 与 input）校验返回结果，不一致视为无效。
- 尚未得到批准时，agent 以 `status: "approval_required"` 和 `approval_requests` 返回给宿主，不把待审命令当作完成。缺少适配器时同样返回 `approval_required`。
- 审批结果发出 `approval.result` 事件；`--bypass` 发出 `approval.bypassed`。

## TUI

```bash
./target/debug/ply tui --record /tmp/ply-tui.jsonl
```

- TUI 把用户输入交给 agent，不直接调用 read / bash 等工具，也不额外传工具白名单。主 agent 默认有 agent / bash / create / edit / memory / read / remove 七个工具，子 agent 去掉 agent 与 memory，读取继承的记忆快照。
- CLI 为 TUI 选择逐次审批策略，读取、写入、命令执行和模型委派都需要确认；有活动会话时由 TUI 回答审批，否则请求保持 pending。
- `ply tui --bypass` 显式跳过人工确认，界面显示 BYPASS；工具授权、子 agent 上限、挂载限制和普通 hook 的硬拒绝仍然生效。

详见 [RFC 0005](rfcs/0005-tui.md)。

## 其他环境

非终端环境通过审批适配器插件接入，模板见 [examples/plugins/approval-adapter](../examples/plugins/approval-adapter/main.ts)。模板只返回 pending：真正的适配器必须等待经过认证的用户或服务的回答，不能依据模型输出或命令里的参数批准。

## Jev 安全检查

`--jev` 启用 Jev，需要在宿主环境配置 `TYPESAFE_API_KEY`。

- Jev 订阅 `bash.beforeExecute`，独立调用 TypeSafe `/v1/systemone` 批量评估命令风险概率，按本地阈值给出放行、高风险或复核建议，不更换主 agent 的模型。
- 未配置密钥、调用失败、高风险或待复核都会请求确认；`--jev` 同时加载审批协调插件。用 `ply tui` 进行真实终端确认，其他入口没有适配器时返回 `approval_required`。
- Jev 的模型用量记录在它自己的 `jev.model_call` 事件里，不计入 agent 的 `total_usage`。

环境模板见 [examples/jev.env.example](../examples/jev.env.example)，设计见 [RFC 0010](rfcs/0010-jev.md)。
