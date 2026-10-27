# Ply 文档

文档分三层：

1. **概览**：仓库首页的 [README](../README.md)，说明 Ply 是什么、怎么跑起来。
2. **指南**：本目录下按主题组织的使用和开发说明，见下表。
3. **契约与决策**：[SPEC.md](../SPEC.md) 是当前有效的系统契约和验收来源；[rfcs/](rfcs/README.md) 记录每项决策的问题、方案和理由。指南与 SPEC 不一致时，以 SPEC 为准。

| 指南 | 内容 |
|---|---|
| [写插件](plugins.md) | 插件目录与三个钩子、插件句柄与加载顺序、向上下文提供内容、内置插件、Skills |
| [命令行](cli.md) | 各命令、常用选项、挂载与记录文件 |
| [架构](architecture.md) | crate 分层、fiber 调度与归属、取消与清理、记录事件 |
| [工具](tools.md) | bash 工具与 Ply sh、执行前 hook、read 分页 |
| [模型接入](models.md) | npm 上的 pi-ai、DeepSeek、MiMo 及其服务选择 |
| [agent](agent.md) | 子 agent 委派与配额、最终报告校验、用量统计、失败恢复 |
| [上下文](context.md) | 上下文预算、配置字段、自动压缩 |
| [审批与安全](approval.md) | 审批协议、TUI、审批适配器、Jev 安全检查 |
| [开发](development.md) | 规范驱动的开发流程、检查命令、提交钩子、代码组织 |
