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

- `npm ci` 必须在 cargo 构建之前：内置插件的 npm 依赖（如 pi-ai）在运行时从仓库根目录的 `node_modules` 读取，缺少时构建失败并提示（[RFC 0013](rfcs/0013-resources-and-npm.md)）。
- `npm run check` 执行严格类型检查、ESLint、文档链接检查和插件可见面检查（插件、示例、SDK 和运行时 prelude 中不能出现 fiber、`ply:lifecycle`、全局 `ctx`、`.loaded` 或已删除的 `ply:models`），覆盖 SDK 声明、插件、示例与 [类型契约反例](../type-tests/plugins.ts)。
- 根 `tsconfig.json` 打开 `skipLibCheck`，因为 `node_modules` 里的第三方 d.ts 在本仓库的配置下无法通过；`packages/sdk` 的声明由 [check-sdk-types.mjs](../scripts/check-sdk-types.mjs) 单独严格检查。
- 根 `tsconfig.json` 自动加载 SDK 声明，编辑器打开仓库即可检查所有插件和示例。
- 涉及内置模块加载的改动要跑完整的 workspace 测试，包括 CLI、MCP、真实 PTY 与目录插件。
- 自动化测试只用 fixture 和本地 HTTP；真实供应商调用需要另行取得授权并保留轨迹。
- [GitHub Actions](../.github/workflows/ci.yml) 在 macOS runner 上对每次推送到 main 和每个 pull request 运行同样的检查（`npm ci`、`npm run check`、`cargo test --workspace --locked`）。

## Node 兼容用例

Ply 对齐的 Node 版本写在根目录的 `.node-version`（当前 24.14.0），`process.version` 和 CI 的 Node 都取自它（[RFC 0014](rfcs/0014-node-compat-suite.md)）。`tests/node/<类别>/` 里每个 `<名称>.mjs` 是一个用例，`<名称>.expected` 是这个版本的 Node 打印的结果；`cargo test -p ply-cli --test node_compat` 用 `ply run` 跑同一批用例并逐个比对。

```bash
npm run compat:record
npm run compat:check
```

- 录制和复核只接受 `.node-version` 指定的 Node，例如用 `nvm use` 切换；CI 每次都会复核。
- 改变已提供的 Node API 行为时，补充或修改对应类别的用例，再重新录制；不要通过改期望值让 Ply 通过。
- Ply 有意不同于 Node 的用例另写 `<名称>.ply.expected`，并在用例第一行写 `// ply-divergence: <理由>`；只有进程模型与沙箱（pid、argv[0]、环境、`HOME`、挂载、原生扩展、插件根与包边界）可以造成差异。`npm run check` 拒绝没有理由的差异，并核对 `@types/node` 与 `.node-version` 一致。

## 运行时的 Node 模块

`crates/ply-jsc/src/node/` 下是运行时自己实现的 Node 模块（[RFC 0015](rfcs/0015-node-io-and-net.md)），每个文件是一个 TypeScript ES 模块，文件路径去掉 `.ts` 就是模块名。

- build.rs 收集这个目录，不需要另外登记。
- 模块之间用相对路径导入；公开的内置模块写作 `node:x`；prelude 的内部函数（编码、`inspect`、`format` 等）从 `ply:runtime` 导入，类型在同目录的 `runtime.d.ts` 中声明。
- 导出用 `@types/node` 的类型约束，例如 `typeof import("node:buffer").atob`、`satisfies Partial<...>`、`implements`，签名与 Node 的声明不符时 `npm run check` 失败。
- 根 tsconfig、ESLint 和 250 行规则都覆盖这个目录；行为由 `tests/node` 的对照用例保障。

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
- 内置插件的模块目录由 Rust 在构建期收集并嵌入二进制；它们的 npm 依赖目前在运行时从仓库的 `node_modules` 读取。构建和运行都不启动外部 Node。
