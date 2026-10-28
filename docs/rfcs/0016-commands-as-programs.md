# RFC 0016: 命令即程序

状态：提议（2026-10-28）。
关联规范：SPEC-TOOLS、SPEC-FIBER、SPEC-PLUGINS。

## 问题与证据

Ply 接管 shell 和 JS 引擎的目的只有一个：模型的每一次文件和进程访问都经过挂载。现状离这个目的还差一段：

- Ply sh 的命令分派顺序是函数 → builtin → 工具 → 宿主程序 → 127（[simple.rs](../../crates/ply-sh/src/exec/simple.rs) 的 `dispatch`）。工具是 [utils/](../../crates/ply-sh/src/utils/) 里约 40 个 Rust 实现（cat、ls、grep、sed、find、sort、head、tail、wc、du……），经 ply-libc 走挂载 IO。
- 工具之外的一切都落到宿主桥（[host_commands.rs](../../crates/ply-jsc/src/host_commands.rs)）：按 PATH 找到可执行文件，用真实磁盘 cwd 和真实文件系统运行。挂载权限对它无效，参数里的虚拟路径也不重写，[docs/tools.md](../tools.md) 写明了这一点。模型用得最多的 git、格式化器、包管理器都在这条路上，所以今天的强管控只覆盖内部工具和重定向。
- 补一个缺失的命令要写 Rust，还要手工对齐 coreutils 的输出格式。这不是"随手就能加"的成本。
- RFC 0013 之后，npm 包已经能在 Ply 的运行时里加载；RFC 0015 第一阶段之后，`stream`、`events`、`Buffer` 齐了。用 TypeScript 写一个命令的条件已经具备，缺的是让 shell 能找到并运行它的机制。

## 方案

### 1. 命令分四种实现，shell 按名字分派

Rust 实现的 builtin 和工具保持不变：确定性强、shell 自己要用的命令留在 Rust 没有坏处。新增的一类是**程序**：缺失的命令用 TypeScript 写成一个程序，由 shell 在自己的 realm 和 fiber 里运行。分派顺序变为：

函数 → builtin → Rust 工具 → **程序** → 宿主程序 → 127

程序排在宿主程序之前：同名时内部实现优先，这正是管控的含义。要明确使用宿主程序时写绝对路径（`/usr/bin/git`），和今天工具的规则一致。

本 RFC 的"命令"指 shell 里的命令。`ply:commands`（RFC 0004）注册的是宿主 CLI 入口，与此无关。

### 2. 一个程序就是一个 Node 风格的应用

程序不是插件钩子，不导出 `onHandle`，它用 Node CLI 的方式和 shell 对话：

| shell 一侧 | 程序一侧 |
|---|---|
| 命令名与参数 | `process.argv`，约定与 `ply run` 相同（`argv[0]` 的差异已在 SPEC-PLUGINS 登记） |
| 导出的变量 | `process.env`。bash 插件在 exec 前已去掉 `*_API_KEY`，密钥规则自动成立 |
| 当前目录 | `process.cwd()`，是挂载命名空间里的路径 |
| 管道输入 | `process.stdin`：一个在缓冲输入之后结束的 Readable；`isTTY` 为 false |
| 管道输出 | `process.stdout`、`process.stderr`、`console`，写入 shell 的 fds，与工具受同一个缓冲上限约束 |
| 退出码 | `process.exitCode` 或 `process.exit(n)`；未捕获的错误与 `ply run` 一样：消息写到 stderr，退出码 1 |
| 生命周期 | 主模块求值完成且没有待调度的工作时结束（与 `ply run` 相同），或 `exit`，或所属 bash 调用超时、取消 |

程序能用的资源与脚本相同：node 内置模块、`fetch`、挂载 fs、npm 包。程序**不**提供 `ply:plugins`、`ply:events`、`ply:commands` 和 `ply:sh`：命令不是扩大模型工具权限的入口，模型不能通过 shell 命令调用 agent 或绕过 bash 的审批边界。文件 IO 自动在挂载里，因为跑的是同一个运行时，不需要额外做什么。

`process.argv`、`process.stdin` 与退出码的行为由 `tests/node/process` 类别的用例按 RFC 0014 的方式与 Node 比对。

### 3. 自己的 realm 和 fiber

每次运行程序创建一个临时 realm，像进程一样隔离：模块状态不跨调用保留，转译缓存是进程级的，所以开销在毫秒级。`Kernel::run_script` 已经做了大半（临时 realm、argv、跑到空闲、收集输出、回收）；差的是它今天不是 fiber，没有 Spawn/Exit，也没有父亲。

程序是 fiber：

- 父亲是这次 bash 调用的 fiber。Spawn 记录程序名和 argv，Exit 记录终态；`--record` 里能看到 `bash → git` 这一层，深度计入 `max_depth`。
- 退出码是程序的结果值，不是 fiber 的终态：程序跑完就是成功，无论退出码。失败只在未捕获错误或加载失败时发生，shell 报告退出码 1 并把消息写到 stderr。
- 取消和超时沿 fiber 树传播：bash 调用的期限到了或被取消，程序 fiber 收到 AbortError，按 SPEC-FIBER 做有界清理，realm 回收。shell 一侧仍以 `timed_out` 表示超时，与今天相同。
- 程序内部创建的定时器、HTTP 请求和将来的句柄都归属这个 fiber，随它回收。

### 4. 程序从哪里找

按名字查找，顺序为：

1. **内置程序**：仓库 `bin/` 目录，`bin/<name>.ts` 或 `bin/<name>/main.ts`。与内置插件同一机制嵌入二进制，模块只能相对导入目录内的文件，npm 依赖从根 `node_modules` 解析（RFC 0013 第一步的开发模式，打包模式随 RFC 0013 一起实现）。遵守 250 行规则，参与 `npm run check`。
2. **npm 包的 bin**：从 shell 的 cwd 逐级向上查找 `node_modules/.bin/<name>`，读取它指向的文件并以该文件所在包为根运行。这与 `npx` 的做法相同，npm、pnpm 和 yarn 都按这个布局安装。prettier、tsc、eslint 这类 CLI 因此直接成为命令，不用自己写。

模型可见的 `bash` 工具说明列出内置程序的名字，和今天列出工具一样。

### 5. `node` 与 `npx`

程序机制本身就是"在 Ply 的运行时里跑一个文件"，所以两个命令不需要程序体：

- `node FILE [ARGS]`、`node -e CODE` 把 FILE 或 CODE 作为程序运行；
- `npx NAME [ARGS]` 等价于按第 4 节第 2 条查找 NAME。

模型常用这两个命令，今天它们都落到宿主桥上，由宿主 Node 在真实文件系统里执行。

### 6. 管道与输出

管道仍按现在的方式缓冲、顺序执行：程序启动时拿到全部 stdin，输出受 shell 的缓冲上限约束，溢出按现有规则返回非零退出码和诊断。流式管道等 RFC 0015 的 stream 与进程句柄落地后另行考虑。

### 不采用的方案

- **把工具迁到 TS**：Rust 工具不需要动；迁移只增加风险，没有收益。
- **TS 命令用运行时内部模块机制（`crates/ply-jsc/src/node/`）实现**：那是运行时自己的代码，不是程序；没有自己的 realm 和 fiber，也无法复用 npm 包的 bin。
- **程序作为插件，导出 `onHandle`**：JSON 契约和 argv、字节流不匹配，也会把模型可见工具的机制交给 shell 命令。
- **缺失的命令继续走宿主**：这正是要解决的漏洞。

宿主桥本身不在本 RFC 的范围内：它的语义不变，只是排在程序之后。是否改成按命令的显式白名单、是否把挂载物化给原生程序，另起 RFC。

## 兼容范围

用户决定（2026-10-28）：

- 确定性的命令留在 Rust；缺失的命令用 TS 实现。
- 一个命令就是一个应用，走自己的 realm 和 fiber。

**不变**

- `bash` 工具的入参、返回值、审批和 hook 边界；`ply:sh` 的 `exec`。
- Rust builtin 与工具的行为和输出格式；宿主桥的语义（PATH、真实文件系统、HOME、进程组回收）。
- `ply run`、`ply:commands`、插件接口、挂载规则、密钥规则。
- 记录文件的格式：程序的 Spawn/Exit 是现有事件类型的新实例，只是多了一层。

**变更**

- 命令分派顺序在宿主程序之前插入程序：与内置程序同名的宿主程序被遮蔽，绝对路径仍可到达宿主程序。
- `node` 与 `npx` 不再到达宿主 Node。
- 新增仓库目录 `bin/`，纳入 tsconfig、ESLint、250 行检查和可见面检查。

## 实施步骤

1. **机制**：内核提供在调用方 fiber 之下运行程序的入口（临时 realm、argv、env、cwd、stdin、输出 sink、退出码、取消）；shell 在工具与宿主程序之间加入程序解析；内置 `bin/` 的嵌入；`node_modules/.bin` 查找；一个最小的内置程序作为用例载体。
2. **`node` 与 `npx`**。
3. **`git`**：基于 isomorphic-git 的内置程序，覆盖模型常用的子命令（status、diff、log、add、commit、branch、checkout），输出格式与真实 git 差分比对。这是最大的漏点，也是对"npm 包当命令"最有分量的验证。

每一步独立验证和提交。

## 验收

- `npm run check` 通过，`bin/` 参与全部静态检查。
- ply-sh：分派顺序与 127/126 的用例；程序遮蔽同名宿主程序，绝对路径到达宿主程序。
- ply-jsc / ply-kernel：
  - 程序的文件读写经过挂载，只读挂载拒绝写入，cwd 是命名空间路径（扩展 [io_control.rs](../../crates/ply-sh/tests/io_control.rs) 的做法）；
  - Spawn/Exit 以 bash 调用为父亲出现在记录里，Exit 的终态按第 3 节；
  - bash 超时与取消在清理期限内回收程序 realm，程序内的定时器和 HTTP 请求随之取消；
  - 程序的 `process.env` 不含 `*_API_KEY`；导入 `ply:plugins`、`ply:sh` 失败；
  - stdin、stdout、stderr、退出码、未捕获错误各一个用例；输出溢出的诊断与工具一致；
  - `node_modules/.bin` 查找用 `tests/` 下的 fixture `node_modules`，与 RFC 0013 的用例同一布局。
- `tests/node/process` 增加 argv、stdin、exitCode 的用例，由 `.node-version` 指定的 Node 录制。
- 重新实现宿主工具的程序（git）用差分测试：真实工具可用时逐例比对输出，不可用时跳过，与 [vs_bash.rs](../../crates/ply-sh/tests/vs_bash.rs) 相同。
- 记录程序的冷、热启动开销。

## 规范同步

实现后更新 SPEC-TOOLS（分派顺序、程序的接口与资源边界、`bin/` 与 `.bin` 查找、`node`/`npx`）、SPEC-FIBER（程序 fiber 的归属与终态）、[docs/tools.md](../tools.md)、[docs/cli.md](../cli.md) 的记录说明，以及 `bash` 工具的 JSDoc；在本 RFC 记录每一步的验证结果。
