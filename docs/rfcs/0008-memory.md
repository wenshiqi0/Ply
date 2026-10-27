# RFC 0008: Persistent memory

状态：已实现。当前有效契约以 [SPEC.md](../../SPEC.md) 为准；本文件保留方案细节、边界与操作示例。

## 问题与决策

显式持久记忆需要跨会话可查、可更新、可遗忘，同时不能绕过上下文归属与权限。选择私有文件存储，由 context 统一注入。

`memory` 是默认插件，支持显式记住、跨会话召回和遗忘；不自动总结历史，也不产生额外的模型调用。公开入口统一为 `import { memory, context } from "ply:plugins"`。

## 使用

在 TUI 中可以直接说“记住我偏好用中文回答”或“忘记之前记录的回答语言”。主模型通过 `memory` 工具调用 `remember`、`recall`、`forget`。只有用户明确要求时才应改变记忆；已有条目先召回，再带 `expected_version` 修改或遗忘。

CLI 也支持显式操作：

```bash
ply memory '{"action":"remember","scope":"user","key":"回答语言","content":"用户偏好用中文回答。"}'
ply memory '{"action":"recall","scope":"all"}'
ply memory '{"action":"recall","scope":"user","key":"回答语言"}'
ply memory '{"action":"forget","id":"<recall 返回的 id>","expected_version":1}'
```

也可使用 `ply call memory '<JSON>'` 或在 TS 中 `await memory({...})`。`recall` 使用精确 `key` 查找，不接收 id 或 query；省略 key 可浏览索引。列表默认返回 8 条、最多 20 条，使用 `offset` 翻页，单条长正文会标明 `truncated`。指定 key 后读取详细正文，长内容使用 `content_offset` / `next_content_offset` 翻页，直到 `has_more_content:false`。默认每页 4096 个字符，结果按 16000 UTF-8 字节进一步收敛；跨页可带 `expected_version` 并核对返回的 id，避免拼接更新或重建后的不同条目。若 user/project 存在同名 key，应显式选 scope。`has_more` / `next_offset` 用于条目列表分页。自动 context 召回仍按当前任务做内部词法排序，不需要模型提供额外查询参数。

`remember` 要求 `key`、`content`；同 scope/key 的相同内容是 no-op，不增加版本。替换不同内容需要当前 `expected_version`，版本不匹配会拒绝而不覆盖。`forget` 只接受 `id + expected_version`，不接收 key 或 scope，由存储层在用户和当前项目中定位该 id；已不存在的 id 返回 no-op。遗忘后的旧 ID 不会被自动恢复；用户以后明确要求重新记住同名 key 时会生成新 ID，因此重放旧 id 的遗忘请求不会删除新条目。

## 作用域与存储

默认根目录是宿主 `~/.ply/memory`，不依赖工作区可写权限，也不随工作区挂载暴露给文件工具。

- `user`：当前宿主用户的个人偏好，跨项目共享。
- `project`：当前仓库事实与经验；默认写入范围。Git 项目用 canonical git common directory 确定身份，因此同仓库 worktree 共享。非 Git 磁盘目录使用 canonical cwd；纯内存工作区不提供 project scope。
- `all`：只用于召回，合并当前项目与用户记忆。

```text
~/.ply/memory/
  user/
    .lock
    CURRENT
    generations/g-<id>/
      index.json
      MEMORY.md
      notes/<entry-id>.md
  projects/p-<project-id>/
    ...
```

`CURRENT` 是单一发布点。每次变更先写完整的新 generation，fsync 后原子替换 CURRENT；读取不会看到半份索引或正文。发布后清理旧 generation，遗忘不会把正文留在当前索引或可召回的旧版本里。清理失败会返回 `cleanup_pending:true`，不声称旧文件已全部清除。遗忘不擦除已经存在的聊天、检查点、录制文件或系统备份。

目录要求由当前用户所有、0700，文件要求 0600；不接受目录或文件符号链接，不擅自 chmod 已有路径。读写持有跨进程文件锁，等待上限两秒并响应所属调用取消；发布前取消不会提交新版本。索引格式为 1，不认识的格式、损坏、正文校验不符或项目身份冲突会明确报错，不自动迁移或悄悄当空存储。项目路径哈希用于命名，并以完整身份复核，不作为安全认证。

每 scope 最多 128 条、总正文 1 MiB，单条正文最多 16 KiB，key 最多 128 个可打印字符。索引保存条目版本、创建/更新时间和原生调用来源。正文是 Markdown 文件；MEMORY.md 是派生索引。请通过插件修改，而非直接编辑 generation 文件，以保持版本和校验一致。

## context 统一注入

memory 的固定使用说明通过 `context.Type.Extensions` 提供。动态数据通过同一套声明式入口配置：

```ts
await context.set({
  type: context.Type.Reference,
  key: "memory",
  content: null,
  provider: "contextEntries",
});
```

`provider` 是当前插件 onLoad 返回的方法名，只能在 onLoad 声明 State/Reference 来源，不能指定数字 order 或冒充来源。方法接收 `{query,cwd}` 并返回 `string | null` 数据，不应递归刷新上下文。context 统一负责调用、排序、版本和消息构建；agent 不读取目录或拼接记忆。

新根会话会等待来源加载，再取得有预算的记忆快照；子 agent 继承父快照。读取失败会注入不可用说明，不让主任务因为记忆故障而失败，也不把故障谎称为“没有记忆”。同一会话不逐轮轮询磁盘。显式 memory 操作后，插件调用 `context.refresh({key:"memory"})`，只刷新所属会话；变化按既有 Reference 规则在安全边界尾部追加，相同内容不产生消息，也不改固定前缀。

恢复快照仍使用 context 格式 2、agent 格式 4。恢复根会话时重新获取动态来源，遗忘后不会从旧快照恢复成有效记忆；历史消息可能仍保留旧文字，最新 Reference 覆盖旧版本。其他普通内容块与固定前缀仍按原快照恢复。

## 权限

默认根模型增加 `memory` 工具，共七个工具；默认子 agent 去掉 `agent` 与 `memory`，保留五个并读取继承的记忆。存储入口还通过原生 realm 身份校验，并拒绝有多层 agent 祖先的记忆变更。模型的显式 memory 调用走现有工具审批，TUI 可以展示原始参数；`--bypass` 由宿主决定并按现有方式审计。直接由用户执行 CLI memory 命令属于显式宿主操作。

记忆是参考数据，不授予工具、文件或委派权限。上述 API 限制不构成操作系统文件沙箱：当前宿主可执行程序仍能按自身权限访问真实宿主文件系统。

## 配置

```bash
ply tui --plugin-options 'memory={"directory":"/absolute/private/memory","max_context_bytes":8192}'
```

`directory` 默认 `~/.ply/memory`，必须是绝对路径；仅由宿主在 onLoad 配置。`max_context_bytes` 是自动注入快照的 UTF-8 预算，默认 8192，范围 1024..32768。先按当前任务词法相关性排序，再优先项目范围和较新条目；自动注入短预览，完整内容按需 recall。

`--no-default-plugins` 可关闭默认插件；嵌入方也可省略 `PluginSource::builtin("memory")`。与其他插件相同，配置通过 `KernelConfig.plugin_options` 传入。读取空存储不会创建真实的记忆目录；首次显式写入才创建。
