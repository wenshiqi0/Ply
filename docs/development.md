# 开发

## 规范驱动

- [SPEC.md](../SPEC.md) 是当前的系统契约和验收来源，开发前先确认其中的行为、类型、权限与验收方式。
- 改变行为、公共接口、存储或恢复格式前，先在 [rfcs/](rfcs/README.md) 写 RFC（[模板](rfcs/TEMPLATE.md)），写明问题、方案、兼容范围和验收条件；实现后同步 SPEC。内部重构也要写明保持哪些契约。
- RFC 记录决策，SPEC 描述当前有效的规范，指南（本目录）解释用法；三者出现偏差时修正文档或实现。
- 开发约定见 [AGENTS.md](../AGENTS.md)。

## 检查

```bash
npm ci --ignore-scripts --registry=https://registry.npmjs.org
npm run check
cargo test --workspace --locked --offline
```

- `npm run check` 执行严格类型检查、ESLint、文档链接检查和插件可见面检查（插件、示例、SDK 和运行时 prelude 中不能出现 fiber、`ply:lifecycle`、全局 `ctx` 或 `.loaded`），覆盖 SDK 声明、插件、示例与 [类型契约反例](../type-tests/plugins.ts)。
- 根 `tsconfig.json` 自动加载 SDK 声明，编辑器打开仓库即可检查所有插件和示例。
- 涉及内置模块加载的改动要跑完整的 workspace 测试，包括 CLI、MCP、真实 PTY 与目录插件。
- 自动化测试只用 fixture 和本地 HTTP；真实供应商调用需要另行取得授权并保留轨迹。
- [GitHub Actions](../.github/workflows/ci.yml) 在 macOS runner 上对每次推送到 main 和每个 pull request 运行同样的检查（`npm ci`、`npm run check`、`cargo test --workspace --locked`）。

## 提交钩子

克隆后启用：

```bash
git config core.hooksPath .githooks
```

钩子对已暂存的 TS/JS 执行 `eslint --fix` 并重新暂存；无法自动修复的错误会阻止提交。只部分暂存的文件只检查暂存区的内容、不自动修复。

## 代码组织

- 一个插件一个目录：`main.ts` 只放钩子入口，`types.ts` 声明输入、输出、`onLoad` 参数和导出接口，策略、存储、渲染、统计等放进按职责拆分的模块。例如 [agent](../plugins/agent/) 由 run / model / tools / policy / report / recovery / usage 分担实现。
- 每个 TS 文件最多 250 个非空、非注释行；类型文件也参与严格检查，不用 `any`、`@ts-ignore` 或跳过声明检查。
- 公共插件类型从 `packages/sdk` 的 `ply:plugins` 暴露。
- 内置插件的模块目录由 Rust 在构建期收集并嵌入二进制，普通构建和运行不依赖外部 Node。
