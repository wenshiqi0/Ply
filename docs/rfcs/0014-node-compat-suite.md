# RFC 0014: Node 兼容用例集与版本对齐

状态：已实现（2026-10-27）。
关联规范：SPEC-PLUGINS、SPEC-TYPES。

## 问题与证据

- RFC 0013 把 Ply 运行时定为 Node 的超集：已提供的 Node API 遵循 Node 语义。但"遵循哪个版本的 Node"没有定义：`process.version` 写死为名义值 `v22.0.0`，`@types/node` 是 22.18，而模块解析的语法检测、require(esm) 又按 22.12 之后的行为实现。
- 现有的 Node 行为测试（[node_outlet.rs](../../crates/ply-jsc/tests/node_outlet.rs)、[node_modules.rs](../../crates/ply-jsc/tests/node_modules.rs)、[web.rs](../../crates/ply-jsc/tests/web.rs)）是手写断言，期望值由我们自己写，无法证明与某个 Node 版本一致，也没有按 API 分类，看不出哪些 API 已有保障。
- 用 Node 24.14.0 实测，当前实现与 Node 有已知差异：`exports` 的 `module-sync` 条件对 import 和 require 都匹配；默认启用 `node-addons` 条件；ESM 导入 JSON 必须带 `with { type: "json" }`，否则报 `ERR_IMPORT_ATTRIBUTE_MISSING`，类型不符报 `ERR_IMPORT_ATTRIBUTE_TYPE_INCOMPATIBLE`。Ply 目前三者都不一致。

## 方案

### 1. 目标版本只定义一处

- 仓库根目录的 `.node-version` 写一个确切版本：`24.14.0`。
- 它是唯一来源：
  - CI 的 `actions/setup-node` 用 `node-version-file: .node-version` 安装同一版本；
  - 运行时在编译期读取它，作为 `process.version`（`v24.14.0`）和 `process.versions.node`；
  - 兼容用例的录制与复核只接受这个版本的 Node；
  - `npm run check` 核对 `@types/node` 的主版本与之相同、次版本不高于它（类型不能声明比运行时更新的 API）。`@types/node` 定为 `24.13.6`。
- 升级版本时改 `.node-version`，重新录制期望值，处理新出现的差异。

### 2. 用例按类别组织

- 目录 `tests/node/<类别>/`，每个用例是一个 `<名称>.mjs`，用 `console.log` 打印确定的结果（通常是一行 JSON）。类别对应 Ply 已提供的能力：`modules`、`path`、`fs`、`buffer`、`events`、`util`、`process`、`os`、`timers`、`child_process`、`web`。
- 每个用例在一份新的临时目录中运行，临时目录是所属类别目录的副本，工作目录就是它；类别目录中的共享 fixture（如 `node_modules/`）随之复制。输出中的临时目录路径统一替换为 `<dir>`。
- 用例只断言 Ply 已支持的 API。未提供的 API 不写用例，导入缺失的内置模块仍报 `ERR_UNKNOWN_BUILTIN_MODULE`。fetch 需要本地服务器，而 Ply 没有 `node:http`，它的网络行为仍由 web.rs 覆盖。

### 3. 期望值来自真实 Node，两头校验

- `node scripts/node-compat.mjs record`：用 `.node-version` 指定的 Node 运行每个用例，把输出写入 `<名称>.expected`。
- `node scripts/node-compat.mjs check`：在 CI 中用同一版本的 Node 重新运行并逐个比对，保证提交的期望值确实出自该版本的 Node。版本不符时直接失败，不跳过。
- Ply 一侧由 cargo 集成测试 [node_compat.rs](../../crates/ply-cli/tests/node_compat.rs) 用 `ply run` 跑同一批用例，与期望值比对；任何已保障的 API 退化都会让测试失败。

### 4. 有意差异必须登记理由

- 只有 Ply 的进程模型和沙箱可以造成差异：固定的 pid、`argv[0]`、精选的环境变量、`HOME` 与临时目录、挂载命名空间、不支持原生扩展、插件根与包边界（`ERR_MODULE_OUTSIDE_ROOT`）。
- 这类用例另有 `<名称>.ply.expected`，Ply 一侧与它比对；用例文件开头必须有 `// ply-divergence: <理由>` 注释，检查脚本拒绝没有理由的 `.ply.expected`。
- 其他差异要修实现，不能通过改期望值让测试通过。

### 5. 本次对齐的行为

- `exports` 的条件为 `node`、`import`/`require`、`module-sync`、`default`。`node-addons` 不匹配：Ply 不加载原生扩展，行为等同 `node --no-addons`，登记为有意差异。
- ESM 静态导入和 `import()` 加载 JSON 时必须带 `with { type: "json" }`；`require()` 加载 JSON 不需要。
- 其余差异以录制结果为准，逐项修正或登记。

### 不采用的方案

- 手写断言按模块分类：实现简单，但期望值是人写的，无法证明与 Node 一致。
- 移植 Node 自己的 `test/parallel`：依赖 Node 内部测试辅助与大量未实现的模块，代价远超收益。

## 兼容范围

用户决定（2026-10-27）：对齐 Node 24 版本线；期望值由真实 Node 录制，CI 用同一版本复核。

**变更**
- `process.version` 从 `v22.0.0` 改为 `v24.14.0`，`@types/node` 从 22.18 升到 24.13.6。
- ESM 导入 JSON 需要 import attribute；`exports` 增加 `module-sync` 条件。
- CI 的 Node 由 `.node-version` 决定。

**不变**
- 插件接口、`ply:` 模块、CLI 命令和存储格式。

## 验收

- `node scripts/node-compat.mjs check` 在 Node 24.14.0 下通过；版本不符时失败。
- `cargo test -p ply-cli --test node_compat` 通过，覆盖全部类别；每个 `.ply.expected` 都有登记的理由。
- `npm run check` 核对 `.node-version` 与 `@types/node`；完整 workspace 测试通过。

## 规范同步

- SPEC-PLUGINS：写明对齐的 Node 版本、用例集的位置与有意差异的规则。
- SPEC-TYPES：`@types/node` 版本规则。
- 开发文档：录制和复核的命令。

## 验证记录（2026-10-27）

- 首批 26 个用例，覆盖 11 个类别；期望值由 Node 24.14.0 录制，`npm run compat:check` 复核通过。
- Ply 首次比对有 13 个用例与 Node 不同，均已修正实现：
  - 模块：`module-sync` 条件、require(esm) 的 `__esModule`、JSON 的 import attribute；
  - path：`basename(p, p)`、`format` 补点；
  - fs：读目录的错误形状、递归 `mkdir` 的返回值（按传入形式返回首个新建目录）；
  - process：`env` 赋值转为字符串、`exitCode`；
  - util：`inspect` 给字符串加引号、`isDeepStrictEqual` 改为真正的深比较；
  - web：`onabort` 按设置时机排序、`structuredClone` 按结构化克隆算法实现。
- 唯一登记的差异是 `modules/addons`：Ply 不匹配 `node-addons` 条件。
- `npm run check` 拒绝没有理由的 `.ply.expected`（已验证），并核对 `@types/node` 24.13.6 与 `.node-version` 一致。
- `cargo test --workspace --offline` 207 个测试全部通过，`npm run check` 通过。
