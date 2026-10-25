# Jev bash 安全插件

`jev` 是可选的 bash.beforeExecute 订阅插件，不注册新的模型工具，也不替换主 agent 的 DeepSeek/MiMo。通过 `--jev` 启用（同时加载通用 approval 协调插件），或手动加载插件。实际用户确认/审批由环境适配器提供，见 [审批协议](approval.md)。

## 原生接口

依据 [TypeSafe 官方 System One API](https://docs.typesafe.ai/api/system-one) 和 [Noul 文档](https://docs.typesafe.ai/primitives/noul)，调用 `POST /v1/systemone`，使用 Bearer 认证，发送 `model`、`state` 和命名的 `questions`；读取 `answers.<name>.noul` 及输入/输出 token 用量。Jev 不生成自由文本，插件不使用 Chat Completions，也不要求模型生成 JSON 或文字解释。

插件一次提交四个独立的 Noul：破坏性操作、敏感信息暴露、安全权限变更、越出工作范围。`state` 只包含待执行的原始脚本、cwd 和内部 sh 环境说明；不读取文件，不发送环境变量内容。脚本中已有的字面量会作为脚本文本发送。过大的 state 转为需要审批，不截断后判为安全。

按照[官方 guardrails 的组合方式](https://docs.typesafe.ai/cookbooks/llm_guardrails)，概率由模型提供，本地代码按配置阈值生成风险建议：

- 所有维度 <= `JEV_ALLOW_MAX_RISK`：allow，执行原始脚本。
- 任一维度 >= `JEV_DENY_MIN_RISK`：高风险建议 deny，并设置 requires_approval:true；用户或审批系统仍可明确批准。
- 其余：review，设置 requires_approval:true。

这里的 decision 是风险建议，不是用户最终决定。bash 将需要确认的结果交给独立审批层；批准后继续原命令，明确拒绝或取消才终止。可用 `wendy tui --jev` 进行终端审批；没有环境适配器时返回 approval_required，而不是最终拒绝。

阈值默认 0.10 / 0.90 是本插件的保守初值，不代表厂商承诺或普适准确率。`reason_codes` 来自触发的维度，`reason` 是本地生成的说明，不冒充模型自然语言解释。Noul 没有单独的 confidence 字段，插件保留原始概率。

## 配置

模板在 [examples/jev.env.example](../examples/jev.env.example)。复制到挂载工作区之外，再填入密钥：

```bash
mkdir -p "$HOME/.config/wendy"
cp examples/jev.env.example "$HOME/.config/wendy/jev.env"
chmod 600 "$HOME/.config/wendy/jev.env"
# 编辑上述文件，填入 TYPESAFE_API_KEY
set -a
source "$HOME/.config/wendy/jev.env"
set +a
```

| 环境变量 | 默认值 | 含义 |
|---|---|---|
| `TYPESAFE_API_KEY` | 自动审查所需 | 由 Rust 宿主读取；未配置则进入人工确认/审批，不注入插件 process.env |
| `JEV_BASE_URL` | https://api.typesafe.ai/v1 | 原生 API base URL，不填完整 /systemone 路径，不支持 URL 内嵌认证 |
| `JEV_MODEL` | jev-latest | 可填模型 ID 或别名；记录响应的实际模型版本 |
| `JEV_TIMEOUT_MS` | 10000 | 单次 HTTP 审查期限，1–120000 ms；还受 bash 剩余 hook 期限约束 |
| `JEV_ALLOW_MAX_RISK` | 0.10 | 放行概率上限，0–1 |
| `JEV_DENY_MIN_RISK` | 0.90 | 高风险建议阈值，须大于放行上限；不是用户最终拒绝 |
| `JEV_MAX_STATE_BYTES` | 16384 | 单次 state UTF-8 上限，可在 256–16384 范围收窄 |

CLI 只转发这些非敏感 JEV 配置。Rust 嵌入时把它们写入 `KernelConfig.env`，密钥仍放宿主环境变量，再加载 `PluginSource::builtin("jev")`。模板默认别名可随服务版本移动；需要固定评估版本时自行填写具体模型 ID。

## 使用

```bash
cargo build -p wendy-cli --locked
./target/debug/wendy call --jev --record /tmp/wendy-jev.jsonl bash '{"command":"pwd; ls"}'
./target/debug/wendy call --jev --record /tmp/wendy-agent-jev.jsonl agent '{"prompt":"只读查看项目目录", "allowed_tools":["bash"], "max_delegation_depth":0}'
```

没有 `--jev` 时保持原有行为。无效配置会启动失败；密钥缺失、接口异常、超时、无效或缺失答案、越界概率均转为 requires_approval，不静默放行，也不代替用户做最终拒绝。此版本不自动重试；限流或过载同样进入确认/审批，调用方也可按[官方错误规则](https://docs.typesafe.ai/api/errors)稍后重新发起审查。

## 结构化结果与用量

每次监听器返回 JSON 对象，包含 `schema_version`、`check_id`、`allow`、`decision`、`risk_level`、`risk_probabilities`、`reason_codes`、`reason`、请求/实际模型、requires_approval、状态、用量和耗时。它出现在回调的 Exit.output 和 `jev_check` 记录中。`ctx.jevStatus()` 可查看该插件实例的检查及用量汇总；unfinished_checks 包括强制取消后未能写完结果的检查，不表示请求仍在运行。

实际 HTTP 审查另写 `model_call_started` / `model_call`，purpose 为 safety，按该 Jev fiber 的 call_index 关联。JeV 审查的 token 会逐层计入活跃祖先 agent 的 total_usage，自身 usage 仍只统计 agent 的调用。开始后被强制取消的调用保留缺失用量标记，不按 0 token 计算。

官方响应只报告 input_tokens / output_tokens，没有缓存或推理分项；这些指标记录为 null，不能编造缓存命中率。包含 Jev 的整树汇总因此可能有完整输入/输出用量，但缓存率为 null；主 agent 自身的缓存率照常保留。失败前已报告的有效用量仍计入汇总，恢复快照保留历史累计。

## 当前范围

只覆盖模型可见 bash 插件的整个调用，尚未覆盖 create/edit/remove 等其它工具。发送请求使用 JSC 插件逻辑和 Rust 原生 JSON HTTP，不启动外部 Node。HTTP 取消随监听器作用域回收，错误响应体不透传到日志。模型概率判断不替代挂载约束、工具权限或 OS 隔离。

测试使用本地模拟官方 API，覆盖自动放行/高风险建议/待审批、无效答案、失败/超时/取消、密钥不泄漏、版本别名、用量及恢复；真实服务须在填好环境变量后另行验证。
