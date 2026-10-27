# 开发流程

- 先阅读 [SPEC.md](SPEC.md)，它定义当前系统契约和验收来源。
- 改变行为、公共 API、存储或恢复格式前，在 [docs/rfcs/](docs/rfcs/) 写明问题、方案、兼容范围和验收条件；实现后同步 SPEC。内部重构也要明确保持哪些契约。
- RFC 记录决策，SPEC 描述当前有效规范；出现偏差应修正文档或实现，不添加未经确认的兼容入口。
- 一个插件一个目录，`main.ts` 保留钩子入口，`types.ts` 声明输入、输出和导出接口；按职责拆分实现。普通源码最多 250 个非空、非注释行。
- 公共插件类型从 `packages/sdk` 的 `ply:plugins` 暴露。类型文件也参与严格检查，不用 `any`、`@ts-ignore` 或跳过声明检查消除诊断；根 tsconfig 的 `skipLibCheck` 只为 `node_modules` 的第三方声明而开，`packages/sdk` 的声明由 `scripts/check-sdk-types.mjs` 严格检查。
- 完成前运行 `npm ci --ignore-scripts`、`npm run check` 和相关 Rust 测试。涉及内置模块加载的重构运行完整 workspace 测试，包括 CLI、MCP、真实 PTY 与目录插件。
- 克隆后运行 `git config core.hooksPath .githooks` 启用提交钩子：对已暂存的 TS/JS 自动 `eslint --fix` 并重新暂存，无法自动修复的错误会阻止提交。
- 自动化测试使用 fixture / 本地 HTTP；真实供应商调用另行取得用户授权并保留轨迹。
