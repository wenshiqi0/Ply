# RFC 0013: 运行时只提供资源，代码依赖走 npm

状态：提议。
关联规范：SPEC-PLUGINS、SPEC-TYPES、SPEC-AGENT、SPEC-TOOLS、SPEC-SKILLS-MEMORY。

## 问题与证据

现在的 `ply:` 模块混了几类不同性质的东西：

| 类别 | 模块 | 现状 |
|---|---|---|
| 资源 | `ply:sh`、`ply:terminal`、`ply:http` | Rust 实现 |
| 内核 | `ply:plugins`、`ply:events`、`ply:commands` | Rust 实现 |
| 外部代码 | `ply:models` | [packages/pi-ai](../packages/pi-ai/build.mjs) 用 esbuild 打成浏览器版 bundle（533 KB），检入 `crates/ply-jsc/src/vendor/pi-ai.js`，再用 `include_str!` 嵌入。fetch、Headers、URL、AbortController、ReadableStream 等只在这个 bundle 的私有作用域里用 polyfill 提供 |
| 外部代码 | `ply:markdown`、`ply:skills` | Rust 解析器（[markdown.rs](../crates/ply-jsc/src/markdown.rs)、[skills.rs](../crates/ply-jsc/src/skills.rs)）加 markdown.js |
| 存储逻辑 | `ply:memory-store`、`ply:checkpoints` | [memory.rs](../crates/ply-jsc/src/memory.rs)（946 行）和 [checkpoints.rs](../crates/ply-jsc/src/checkpoints.rs) 在 Rust 里实现私有权限、flock、原子 generation 和 git common-dir 识别 |

由此带来的问题：

- 升级外部依赖或新增 provider 都要改 Rust 运行时和打包脚本。插件不能像普通 JS 那样 import npm 包，裸模块名直接报 "importing packages is not supported yet"。
- 存储格式和策略本是 memory、agent 插件自己的业务，却写在运行时里，插件要改存储就得改 Rust。
- Web 全局只给 bundle 用，普通插件拿不到 `fetch`。
- 密钥有两套做法：`models.credentials` 把环境变量里的 key 交给 JS；`ply:http` 则由宿主注入、不回到 JS。

## 方案

### 原则

运行时只提供两类能力：

- **资源**：进程环境（`process.env`、argv、cwd）、进程执行（`ply:sh`、child_process）、文件（fs）、网络（`fetch`）、终端（`ply:terminal`）。
- **内核**：`ply:plugins`、`ply:events`、`ply:commands`。

外部代码（模型 SDK、解析器）通过 node 模块解析从 `node_modules` 引入；存储由插件在 fs 上实现；密钥通过环境变量注入。Web 标准对象属于运行环境，与 `console`、`Buffer` 同类，不作为 `ply:` 模块暴露。

Ply 运行时以 Node 为基线，可以作为 Node 的超集：Node 已有的 API 按 Node 语义提供；Node 缺少的资源能力（例如文件锁）作为扩展加在对应的 Node 对象上，不另起 `ply:` 模块。

### 1. node 模块解析

- 裸模块名（`pkg`、`@scope/pkg`，可带子路径）按 Node 的算法解析：从导入方所在目录逐级向上查找 `node_modules/<pkg>`。
- 读取 package.json 的 `exports`，条件依次为 `node`、`import` 或 `require`、`default`；没有 `exports` 时用 `main`，再退回 `index.js`。
- 同时支持 ESM 与 CommonJS：
  - CJS 模块可以使用 `module`、`exports`、`require`、`__filename`、`__dirname`；
  - ESM 导入 CJS 时，`default` 为 `module.exports`；
  - `type: "module"`、`.mjs`、`.cjs` 按 Node 规则区分；
  - 可以导入 JSON。
- 包内的相对导入不能越出该包，插件内的相对导入不能越出插件目录；`node_modules` 只读。
- 不支持原生扩展（`.node`）。导入运行时未实现的 node 内置模块时，报 `ERR_UNKNOWN_BUILTIN_MODULE`。
- 内置插件、目录插件和脚本使用同一个解析器。

### 2. 开发与打包

- **依赖声明**：运行时依赖写在根 package.json 的 `dependencies`，版本锁在根 package-lock.json，与 SPEC-TYPES 一致。删除 `packages/pi-ai` 及其独立的锁文件。
- **开发模式**：内置插件从源码目录加载，依赖从磁盘上的 `node_modules` 解析，事先需要运行 `npm ci --ignore-scripts`。
- **打包模式**：构建期从每个内置插件的入口出发，用同一个解析器遍历静态导入（包括字面量的 `import()`），把可达的模块文件、所在包的 package.json 和许可证文件嵌入二进制。运行时把它当作一个只读命名空间读取，解析规则与开发模式完全相同。这与 Electron 把 node_modules 打进 asar 是同一种做法。
- **模式切换**：debug 构建为开发模式，release 构建为打包模式。
- **构建约束**：
  - 非字面量的动态 `import()` 在打包后可能找不到目标，构建期会列出所有这类导入。
  - cargo 构建不运行 npm 或 node，只读取已经安装好的 `node_modules`；依赖缺失时构建失败，并提示先运行 `npm ci`。
  - 第三方许可证由构建期从被嵌入文件所属的包中收集，取代现在 build.mjs 里的汇总逻辑。

### 3. 网络：fetch

- 提供全局 `fetch`、`Headers`、`Request`、`Response`、`URL`、`URLSearchParams`、`AbortController`、`AbortSignal`、`ReadableStream` 和 `FormData`（即现在 pi-ai bundle 需要的那一组）。底层沿用 Rust HTTP/TLS 的流式读取和体积上限。
- 请求归属发起它的那次调用；调用被取消时，请求以 `AbortError` 结束，与 RFC 0012 一致。
- pi-ai 及其依赖实际用到的其他标准全局（例如 `crypto.getRandomValues`），按加载测试的结果补齐。这些 Web 对象的实现可以复用 npm 上的 polyfill，由运行时在构建期嵌入。
- 删除 `ply:http`（`requestJson`、`hasCredentials`），以及 http.open、http.headers、http.read、http.abort 这几个 op 的 JS 适配层。

### 4. 密钥：环境变量

- 插件从 `process.env` 读取密钥，默认变量为 `DEEPSEEK_API_KEY`、`MIMO_API_KEY`、`TYPESAFE_API_KEY`；provider 的 `apiKeyEnv` 选项可以改用其他变量名。读到的密钥作为参数交给 pi-ai，或写进 `fetch` 的请求头。
- 删除 `models.credentials` 和 `http.auth_configured` 两个宿主 op。
- 密钥会出现在插件的 JS 中。模型调用现在已经是这样；jev 原本"密钥经 `ply:http` 注入、不回到 JS"的保证随之取消。密钥仍然只来自宿主的进程环境，不从插件输入或模型输出读取。

### 5. 存储建在 fs 上，fs 提供文件锁

**fs 补齐**
- `open` 返回 FileHandle，支持 `read`、`write`、`sync`、`close`。
- `mkdir`、`writeFile`、`open` 支持 `mode` 参数；新增 `chmod`。

**文件锁**（Node 没有文件锁，这是 Ply 对 Node 的扩展，挂在 FileHandle 上，与 Deno 的 `FsFile.lock` 类似；SDK 通过声明合并，为 `node:fs/promises` 的 FileHandle 补上类型）
- `handle.lock({ wait = true })` 返回 `Promise<boolean>`，用于取得 OS 排他咨询锁（flock），跨 Ply 进程有效。
  - `wait` 为 false 时，锁已被占用则直接返回 false。
  - `wait` 为 true 时一直等待，等待可以被取消（`AbortError`）。
- `handle.unlock()` 释放锁。关闭句柄、进程退出、所属调用被回收时也会自动释放。
- 只在磁盘挂载上可用；内存和 SQLite 后端返回 `ENOTSUP`。

**插件私有状态目录**
- 内核为每个插件挂载 `<state>/<插件名>`：默认是 `~/.ply/<插件名>`，磁盘、读写、权限 0700。路径通过 `process.env.PLY_STATE_DIR` 告知插件。

**memory**
- memory.rs 的逻辑迁到 memory 插件：私有权限、锁、CURRENT 指针与原子 generation、git common-dir 识别、`cleanup_pending`。
- 状态目录就是 `~/.ply/memory`，存储格式不变。删除 `ply:memory-store`。

**checkpoints**
- 迁到 agent 插件，状态目录为 `~/.ply/agent/checkpoints`；claim 持有锁，直到 finish 或进程退出。恢复格式 4 不变。
- 旧 `~/.ply/checkpoints` 下的记录不迁移。删除 `ply:checkpoints`。

### 6. 模型层与解析器

- agent、deepseek、mimo 直接 `import` `@earendil-works/pi-ai` 及其 providers 子路径。现在 entry.js 里的 describe 和 request 适配逻辑迁到 agent 插件的一个模块中；deepseek、mimo 仍然只提供模型配置。
- jev 改用 `fetch`。
- 删除 `ply:models`、`models.source` op、`vendor/pi-ai.js` 和 `packages/pi-ai`。
- `ply:markdown` 改为 tui 插件使用 npm 上的 markdown 解析包；`ply:skills` 改为 skill 插件自己解析 SKILL.md 的前置元数据。删除 markdown.rs 和 skills.rs 的宿主 op。具体选用哪个包在实现时确定，并在根锁文件中锁定版本。
- 已经确认的导入情况：pi-ai 主入口在 Ply 的解析条件下会用到 `node:fs`（`utils/provider-env.js`），运行时已经提供；`node:http`、`node:readline` 只出现在 cli 和 OAuth 子路径中，内置插件不会导入这些路径。

### 不采用的方案

- 继续把外部库包装成 `ply:` 模块。
- 由 `fetch` 按目标主机注入密钥：这等于在 IO 层加策略；按用户决定，密钥走环境变量。
- 用 `open(path, "wx")` 建锁文件来代替 OS 锁：进程崩溃后会留下失效的锁。
- 打包整个生产 `node_modules`：AWS、Google 等 provider SDK 会全部进入二进制；改为只嵌入按导入图可达的文件。

## 兼容范围

用户决定（2026-10-27）：
- 运行时只提供资源层能力。
- 外部代码依赖通过 node 模块解析引入：开发模式使用 `node_modules`，打包时随应用嵌入，与 Electron 相同。
- 密钥通过环境变量注入。
- 存储的跨进程互斥使用 fs 提供的文件锁。
- Ply 运行时可以作为 Node 的超集，文件锁以 Node 扩展的形式提供。
- 以上原则写入 SPEC。

**破坏性变更**
- 删除 `ply:models`、`ply:memory-store`、`ply:checkpoints`、`ply:http`、`ply:markdown`、`ply:skills`，以及 [packages/sdk/modules](../packages/sdk/modules/models.d.ts) 中对应的声明。
- 新增全局 `fetch` 等 Web 对象；fs 新增 FileHandle、`mode`、`chmod`、`lock` 和 `unlock`；新增 `process.env.PLY_STATE_DIR`。
- cargo 构建前必须先运行 `npm ci --ignore-scripts`。以前 vendor bundle 检入仓库，不需要这一步。
- 检查点改存到 `~/.ply/agent/checkpoints`，旧位置的记录不迁移。
- jev 的密钥会进入 JS。

**不变**
- CLI 的命令与参数。
- memory 的存储位置与格式；agent 恢复格式 4、context 恢复格式 2。
- 模型调用不启动外部 Node 进程。
- DeepSeek、MiMo 的选择方式，以及 `apiKeyEnv`。

本 RFC 与 RFC 0012 相互独立；0012 中 `AbortError` 的取消语义同样适用于 `fetch` 和锁等待。

## 验收

**自动检查**
- `npm ci --ignore-scripts`、`npm run check`、`cargo test --workspace --locked --offline`（开发模式）。
- 打包模式测试：验证内置插件能从嵌入的命名空间加载，包括 CLI、MCP 和真实 PTY。

**针对性回归**
1. 解析：覆盖裸模块、scope 包、子路径 exports、条件选择、CJS 与 ESM 互操作、JSON、逐级向上查找；未实现的内置模块报错；相对导入越界被拒绝。同一组 fixture 在开发和打包两种模式下结果一致。
2. 打包：只嵌入可达文件；字面量 `import()` 被跟随；非字面量的动态导入在构建期列出；缺少 `node_modules` 时构建失败并给出提示。记录二进制体积的变化。
3. fetch：用本地 HTTP 覆盖 JSON、SSE 流、体积上限、取消（`AbortError`）和并发隔离，沿用 [model_runtime.rs](../crates/ply-jsc/tests/model_runtime.rs) 的场景。
4. 密钥：环境变量缺失时明确报错；`apiKeyEnv` 改名生效；fixture 测试不发出真实请求。
5. 文件锁：
   - 同一进程的两个调用、两个 Ply 进程分别争用同一把锁；
   - `wait: false` 时返回 false；`wait: true` 时等待，且可以被取消；
   - 关闭句柄、进程退出、调用被回收后锁都会释放；
   - 非磁盘挂载返回 `ENOTSUP`。
6. memory：现有 memory 集成测试全部通过；与迁移前的存储互相可读；权限为 0600/0700；`cleanup_pending` 行为不变。
7. checkpoints：claim 跨进程互斥；finish 后释放；恢复格式 4 不变。
8. check 脚本确认插件中不再出现已删除的 `ply:` 模块。

**真实供应商**
本 RFC 改变了模型连接的路径。按 AGENTS.md，需要另行取得授权，做一次 DeepSeek 和 MiMo 的实际调用并保留轨迹。

## 规范同步

- **SPEC-PLUGINS**：运行时边界原则已在本轮写入，并标注为迁移中；实现后补充 node 解析、开发模式与打包模式。
- **SPEC-TYPES**：删除 SDK 中对应的模块声明；npm 依赖记录在根锁文件。
- **SPEC-AGENT**：模型层改为 npm 上的 pi-ai 加 `fetch`；密钥来自环境变量。
- **SPEC-TOOLS**：`fetch`；fs 的权限与文件锁。
- **SPEC-SKILLS-MEMORY**：memory 改在插件内实现，锁与权限的要求保持不变。
- **验收矩阵**：模型运行时、持久化记忆、失败恢复三行改为引用新的测试。
