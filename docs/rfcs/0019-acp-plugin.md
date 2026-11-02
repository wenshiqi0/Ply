# RFC 0019：ACP 协议插件与通用命令 IO

状态：已实现，待发布 · 2026-11-02

## 问题

MCP 把 Ply 暴露为工具集合，CLI 中的协议循环又硬编码了 agent 的宿主方法。
Ply 是 agent harness，应以 Agent Client Protocol 向客户端提供会话、提示、工具进度、审批和取消。
协议与业务路由属于插件，不能让 CLI 或内核识别 ACP 方法、会话或 agent 方法。

## 决策与边界

- 删除 Rust MCP 服务和 `ply mcp`、`--host-chat`，不保留协议兼容入口。
- `plugins/acp` 通过 `ply:commands` 注册 `acp`，使用 Node 标准流提供 ACP v1 stdio 服务。
- CLI 仅为宿主调用的注册命令接通通用标准流；插件命令声明是否接管流。
  其他插件的输出继续隔离，日志不能混入协议流。内核不硬编码协议名或 CLI 保留名。
- 运行时为插件调用提供可取消的 awaitable（`cancel()`），取消自己持有的调用能力，
  不暴露调度身份。取消实际终止所 await 的调用树，并保留现有清理边界。
- ACP 实现初始化、会话创建/加载、文本和图片提示、模型与思考配置、公开回复、
  工具开始/结果、双向权限请求和会话取消。会话保存在宿主 `~/.ply/plugins/acp`，
  目录 0700、文件 0600；不保存密钥，不向客户端或模型工具公开恢复快照。
- CLI 配置通用插件状态根目录；每个插件通过自己的 `/ply/state` 挂载和
  `PLY_DATA_DIRECTORY` 访问私有存储。IO 子视图共享原有工作区的实时挂载，
  仅私有挂载彼此隔离；内核与运行时不识别会话格式或协议名。
- 同一进程一次执行一个 prompt；不同会话保存独立上下文。未实现外部 MCP server 接入，
  对非空 `mcpServers` 明确拒绝；不声称支持音频、客户端文件/终端委托或未知协议能力。
- ACP 会话 cwd 必须等于进程的工作目录；其他目录由另一个 Ply 进程建立，
  不让协议请求扩大启动时的挂载权限。
- agent 保留通用模型目录与连续对话导出；宿主审批队列和协议映射迁到 ACP 插件。
  审批仍执行原有策略，客户端只可应答当前未决请求。
- Kite 的 Ply 适配同步改为 ACP；默认启动 `ply acp --approval-mode ask`，
  保留模型、图片、工具、审批、取消和会话恢复。旧 MCP 命令配置需迁移到新命令。

## 验收

- CLI 和内核无 MCP/ACP 路由分支；ACP 是可发现的插件命令，不成为模型工具。
- 真实二进制覆盖协议校验、连续会话与重启恢复、配置选择、图片、工具进度、
  审批拒绝/批准/取消、取消后继续服务、EOF 清理和 stdout 隔离。
- 通用标准流与可取消调用有独立测试；既有插件、CLI、真实 PTY 和 Node 兼容测试通过。
- `npm ci --ignore-scripts`、`npm run check`、完整 Rust workspace 测试及 Kite 检查通过。
- 仅使用本地 HTTP 模型夹具；真实供应商验证另行记录。

## 协议来源

- https://agentclientprotocol.com/protocol/v1/initialization
- https://agentclientprotocol.com/protocol/v1/session-setup
- https://agentclientprotocol.com/protocol/v1/prompt-turn
- https://agentclientprotocol.com/protocol/v1/tool-calls
- https://agentclientprotocol.com/protocol/v1/session-config-options

## 验证记录（2026-11-02，Linux）

- `npm ci --ignore-scripts`、`npm run check` 通过；完整 `cargo test --workspace --locked`：215 项通过，包含 Node 兼容、真实 PTY、协议及目录插件。
- ACP 端到端覆盖协议错误恢复、权限拒绝/批准、取消真实子工作、EOF 清理、图片、工具、公开历史回放和私有持久上下文。
- 官方 `@agentclientprotocol/sdk` 2.2.0 客户端验证 initialize、session/new、配置修改及 session/load。
- Kite 新适配器连接本次 Ply 二进制及本地 HTTP 模型，首轮、进程重启与连续上下文通过；无真实供应商请求。

当前实现是 ACP v1 会话能力子集，尚未覆盖规范要求的外部 stdio MCP 消费及不同 cwd 的会话创建，
不宣称完整 ACP v1 符合性。协议请求不能扩大宿主挂载；这两项需要后续显式扩展资源与工具能力。
