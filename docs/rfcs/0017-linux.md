# RFC 0017: Linux 支持

状态：已实现
关联规范：SPEC-PLUGINS、SPEC-FIBER、验收矩阵

## 问题与证据

Ply 目前只允许在 macOS 编译：`ply-jsc` 无条件链接 JavaScriptCore
framework，内存看门狗使用 `proc_pid_rusage`，CI 也只有 macOS。
用户要求支持 Linux；Linux 的 JavaScriptCoreGTK 提供同一套 JSC C API。
引擎使用官方的 [`javascriptcoregtk-4.1` pkg-config 接口](https://webkitgtk.org/reference/jsc-glib/2.38.4/index.html)，
看门狗和 rejection callback 继续使用 [JSC 的 C API](https://github.com/WebKit/WebKit/blob/main/Source/JavaScriptCore/API/JSContextRefPrivate.h)。

## 方案

- macOS 继续链接系统 JavaScriptCore framework；Linux 通过 `pkg-config`
  查找并链接 `javascriptcoregtk-4.1`。缺少开发包时给出安装提示。
- 进程内存水位在 macOS 使用现有 physical footprint，在 Linux 使用
  `/proc/self/statm` 的当前 resident pages 乘以系统页大小。继续统计整个
  进程，不改成单 realm 预算，也不使用只增不减的历史峰值。
- CLI、脚本、MCP、插件、取消清理、文件锁与真实 PTY 在两个平台运行
  同一套 workspace 测试；Node 兼容用例继续对齐 `.node-version`。
- CI 同时覆盖 macOS 与 Ubuntu；Linux 安装 `pkg-config` 和
  `libjavascriptcoregtk-4.1-dev`。README 和开发指南说明构建依赖。

## 兼容范围

保留现有 macOS 链接方式、CLI 参数、插件接口、SDK、挂载权限、存储与
恢复格式。`process.platform`、`process.arch` 与 `os` 继续报告真实宿主。
新增 Linux 平台支持，其他平台仍在编译期明确拒绝。运行时仍不启动
外部 Node，内置插件仍从构建仓库的 `node_modules` 读取 npm 依赖。

## 验收

- 在 Linux 上运行 `npm ci --ignore-scripts`、`npm run check`、
  `npm run compat:check`、`cargo test --workspace --locked` 和 CLI 构建。
- 验证进程内存采样非零；现有内存预算、CPU 超时、取消清理、CLI、MCP
  与真实 PTY 测试全部运行，不跳过平台失败。
- CI 保留 macOS 全套检查；本机 Linux 验收不能代替 macOS 实机结果。
- 自动化只调用 fixture 与本地 HTTP，不调用真实模型供应商。

## 规范同步

已同步 SPEC、RFC 索引、README、开发指南及 CI；不改变 SDK 类型。

## 验收记录（2026-11-02）

- 环境：Debian 13 x86_64、JavaScriptCoreGTK 2.52.6（4.1 ABI）、
  Rust 1.97.1、Node 24.14.0。开发包解压到本机临时前缀，通过
  `PKG_CONFIG_PATH` 查找，链接系统已安装的同版本共享库。
- `npm ci --ignore-scripts`、`npm run check` 通过；
  `npm run compat:check` 的 36 个用例全部匹配固定版本 Node。
- `cargo test --workspace --locked`：209 项测试通过，0 失败、0 忽略，
  包含真实宿主平台、内存预算、取消清理、CLI、MCP 与 14 项真实 PTY 测试。
  本机验收将构建并发、Tokio worker 和测试线程数设为 2，并关闭 dev/test
  调试信息以控制链接内存；没有跳过测试。
- `cargo build -p ply-cli --locked --offline` 通过，生成 Linux CLI；
  `ply --help` 正常，动态链接解析到系统 JavaScriptCoreGTK 共享库。
- macOS 全套检查保留在 CI 矩阵中，本机没有运行 macOS 验收。
