# 会话权限策略

状态：已实现。用户在 Kite 模型设置中显式选择会话权限策略。

## 问题与方案

ACP 只有启动时的 `--approval-mode`，客户端不能查看、修改或恢复当前会话的审批策略。新增 `session/set_config_option` 的 `approval_mode` 选项，原生值为 `ask`、`risk`、`bypass`，类别为 `permissions`；随其余会话配置返回并持久化。

approval 插件新增可信宿主导出 `setMode`。ACP 在开始每次 prompt 前设置该会话的策略；活动 prompt 中配置修改仍被拒绝。此入口不注册为模型工具，模型不能修改策略。bypass 仍记录 `approval.bypassed`，不改变挂载、工具白名单、硬拒绝或 Jev 检查。

格式 1 会话新增可选存储字段 `approvalMode`；既有文件缺失时采用显式启动策略，未配置时为 ask。CLI 行为和审批回包身份保持不变。策略修改不能批准已经挂起的请求。

## 验收

- ACP 返回真实当前策略、只接受三个枚举值，拒绝活动 turn 的变更。
- 更换策略、切换会话、重启恢复后保持各会话选择。
- ask 等待审批；risk 继续由评估监听器决定；bypass 记录真实宿主授权；硬拒绝仍阻止执行。
- 本地模型和插件夹具验证，无真实供应商请求。

## 验证（2026-11-02）

`npm ci --ignore-scripts`、`npm run check`、`cargo test --workspace --locked --offline` 全部通过，包含完整 Node 兼容与真实 PTY 用例。新增 ACP 测试覆盖策略枚举、会话隔离、重启恢复和活动审批时拒绝修改；内核测试覆盖 ask/risk/bypass 切换、真实工具门禁和硬拒绝保持。只使用本地模型/插件夹具。
