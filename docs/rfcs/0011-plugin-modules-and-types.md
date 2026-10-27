# RFC 0011: 插件模块、类型与规范驱动开发

状态：已实现。
关联规范：SPEC-PLUGINS、SPEC-TYPES；其余行为保持现行 SPEC。

## 问题与证据

原 agent/main.ts 671 行、context/main.ts 756 行、tui/main.ts 386 行。SDK 仅有一个未自动包含的 globals.d.ts，已知插件大量退化为动态类型；根目录没有 tsconfig、lint 命令或锁定的检查依赖。

内核原本只把每个内置插件的 main.ts 嵌入二进制；签名读取只理解当前入口声明的类型，导入类型会变为 unknown。因此仅移动文件会让安装包无法加载相对模块或丢失工具 schema。

## 方案

- SPEC 统一当前契约，rfcs 保存方案和理由；原 docs 直接迁移，README 保留快速使用与导航。
- 插件按职责拆分，types.ts 作为输入/输出/导出接口来源；SDK 用这些类型声明已知句柄，不复制一份接口。
- Rust 构建期自动收集内置插件目录，不依赖 Node 打包；运行时仍用 JSC 模块加载。当前 import 复制绑定，因此可变标量留在所属模块，通过方法或稳定对象共享，不能拆成跨模块可变标量引用。
- 签名读取通过只读 code namespace 解析相对类型模块，保留描述、字段说明、可选项和递归收敛行为。
- 根 tsconfig 让编辑器和 CI 使用相同声明；npm 锁定 TypeScript/ESLint 版本，严格检查包括 SDK 与示例。
- 为错误参数组合提供类型反例；为多文件内置插件、目录插件和 schema 保留提供回归。

不采用把所有模块重新手写拼成单个字符串的方案；那会失去模块语义和共享状态边界。也不通过 any、skipLibCheck 或全局屏蔽诊断来消除报错。

## 兼容范围

用户已确认保留 CLI、插件参数与 ply:plugins 公共接口，允许直接重组内部文件和文档路径。现有 Embedded 单文件构造保留；内置目录使用新增的 Bundled 变体，遍历 defaults 的 Rust 调用方需处理该变体。存储和恢复格式不变。旧文档路径删除，不留别名或跳转文件。

## 验收

- npm run check：严格类型、lint、规范链接和 RFC 导航。
- cargo test --workspace --locked --offline：fixture 模型、目录/内置模块、权限、恢复、CLI/MCP 与真实 PTY。
- 正向和负向类型契约验证工具参数、返回值及 memory selector。
- 不运行真实供应商模型调用；本次为行为保持的重构。

## 规范同步

实现与 SDK、SPEC、README 同步；最终结果在完成检查后记录。

## 验证结果（2026-10-27）

- `npm ci --offline --ignore-scripts` 可从锁文件重装；`npm run check` 通过，包括 strict、声明检查、ESLint 和文档链接。
- SDK 入口可以独立加载，类型反例验证错误参数不能通过静态检查。
- workspace 共 184 项自动化测试通过，包含 12 项真实 PTY；新增四项模块回归覆盖内置/目录源、类型别名、传递导入、共享状态、递归终止和路径拒绝。
- 重构前后 `ply tools` 输出 JSON 完全一致，工具描述、schema、顺序与参数包装方式保留。
- 14 个插件都有 types.ts，最大源文件 186 行；模型运行仍在 JSC，无新增运行时 Node 依赖。
- 本轮使用 fixture 与本地 HTTP，没有发起真实供应商请求。
