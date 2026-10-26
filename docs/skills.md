# Skills

`skill` 是提供模型上下文的内置插件，使用 [Agent Skills 的 SKILL.md 格式](https://agentskills.io/specification)及其[渐进加载方式](https://agentskills.io/client-implementation/adding-skills-support)。模型起初看到名称、说明和文件路径，需要使用时通过已有的 `read` 工具读取完整指令，再按需读取引用资料。

## 插件参数

额外目录由 skill 插件的 `onLoad` 接受。CLI 通过通用的 `--plugin-options NAME=JSON` 将 JSON 原样交给对应插件；字段含义和校验由插件负责。

```bash
ply tui --plugin-options 'skill={"paths":["./examples/skills","./team-skills"]}'
ply tools --plugin-options 'skill={"paths":["./examples/skills"]}'
```

`--plugin-options` 适用于 tools、call、run、mcp、tui 和插件注册的顶层命令。每个插件传一份完整 JSON；不同插件可重复该参数，同名重复或未加载的插件名会报错。`tools` 输出插件和工具 schema，skill 的 `callable` 为 false。

skill 自己接受的参数：

```ts
export async function onLoad(options: {
  paths?: string[];      // 额外的 SKILL.md、技能目录或集合
  directory?: string;   // 发现基目录，默认命令启动目录
} = {}) {
  // 校验参数，发现目录，并提供模型上下文。
}
```

`paths` 支持单个 `SKILL.md`、包含该文件的技能目录、或含多个技能目录的集合。集合只扫描直属子目录。相对路径以 `directory` 为基准，仍必须在已挂载的命名空间内。外部目录使用已有的挂载机制显式开放，例如：

```bash
ply tui --mount disk:. --mount /team=disk:/path/to/team-skills:ro \
  --plugin-options 'skill={"paths":["/team"]}'
```

CLI 为 `~/.agents/skills` 建立默认只读挂载，并把可见路径通过 `PLY_USER_SKILLS_DIRECTORY` 交给 skill 插件；其中包含 SKILL.md 的直属目录符号链接会各自建立只读挂载，不开放链接目标的父目录。已有的显式挂载保留原来源与权限。其他额外目录仍需显式挂载，元数据解析、去重和上下文构建由插件负责。

`--cwd` 改变 agent 的工作目录；如果启动目录通过磁盘挂载映射到其他命名空间路径，宿主会向插件提供对应路径。自定义挂载没有开放启动目录时，该目录不可读，可通过插件的 `directory` 指定已挂载的位置。未加载 skill 插件时不自动挂载用户 skills。

Rust 嵌入方在 `KernelConfig` 配置同一个入口：

```rust
let mut config = KernelConfig::new(io, "/work");
config.plugin_options.insert("skill".into(), json!({
    "paths": ["/shared/skills"],
    "directory": "/work"
}));
let kernel = Kernel::new(config);
for plugin in PluginSource::defaults() {
    kernel.load(plugin).await?;
}
kernel.ready().await?;
```

参数传递是所有插件通用的：`onLoad(options)` 获得该插件的 JSON；未指定时传 `{}`。原有无参 `onLoad()` 写法继续可用。skill 的参数由它自己解释，没有添加 agent 工具参数。Rust 嵌入方如需用户级 skills，应自行建立只读挂载，并在 `config.env` 中设置可见的 `PLY_USER_SKILLS_DIRECTORY`；插件不会自行访问宿主文件系统。

## 自动发现与上下文

默认从命令启动目录发现以下位置，再读取 CLI 提供的用户 skills 目录；不向项目父目录搜索：

```text
.agents/skills/<name>/SKILL.md
skills/<name>/SKILL.md
SKILL.md                         # 当前目录本身是一个 skill
~/.agents/skills/<name>/SKILL.md  # 用户级 skills，目录不存在时跳过
```

skill 在 onLoad 中发现元数据，随后直接向 context 写入 Extensions 模板：

```ts
import { context } from "ply:plugins";

await context.loaded;
const catalog = await discoverSkills();
await context.set({
  type: context.Type.Extensions,
  key: "skills",
  content: catalog.skills.length ? skillContext(catalog) : null,
});
```

agent 不调用 skill 的发现、渲染或校验方法。context 在新建对话时冻结 Extensions 内容，在子 agent 中复制，在快照中持久化；恢复不以新的技能配置覆盖旧对话。顺序、固定前缀和增量更新规则见 [Context](context.md)。

模型仍使用原有 agent/bash/create/edit/read/remove 六个工具，默认子 agent 移除 agent，保留五个。skill 不注册模型工具。

优先使用插件 paths 中的显式来源，再使用项目自动发现项，最后使用用户级 skills；同名保留第一个并记录诊断。无效的自动发现项或集合内技能产生诊断；显式文件无效、额外路径不存在时，onLoad 失败。可信宿主可通过 `import { skill } from "ply:plugins"`、等待 `skill.loaded`，读取 `skill.catalog` 查看元数据和诊断。

技能正文由模型通过 read 分页读取到 has_more 为 false，遇到截断时缩小行范围；读取和审批跟随现有工具流程。

## SKILL.md

```markdown
---
name: code-review
description: >-
  Review architecture and correctness with source evidence.
  Use when the user requests a code or architecture review.
---
# Code review

1. Read the relevant source files before drawing conclusions.
2. Cite exact files and explain the impact of each finding.
3. Follow the user's read-only and testing constraints.
```

`name` 必须与所在目录名一致，使用 1–64 个小写英文字母、数字和单个连字符。`description` 是 1–1024 字符的说明；支持 YAML 引号、注释、折叠和多行内容。`metadata` 保留 JSON 兼容的嵌套数据，例如 requires.bins / requires.skills；发现不执行这些依赖声明。`license`、`compatibility`、`allowed-tools` 可解析，其中 `allowed-tools` 只是文档元数据，不授予权限。

单文件最多 64 KiB，frontmatter 最多 16 KiB；额外路径最多 32 个，目录最多 128 个技能、64 KiB 元数据，每个集合最多 2048 个条目、每次发现最多检查 512 个候选文件。不递归遍历集合。

## 权限与验证

宿主选择技能来源属于插件启动配置；发现阶段只向模型提供元数据。模型通过 read 读取正文，TUI 默认逐次审批，`--bypass` 按既有宿主策略记录跳过。技能内容不修改工具授权、挂载、委派权限或子 agent 配额，也不会自动执行 scripts。

`ply:skills.parse(source)` 只解析字符串，不执行文件、网络或脚本。源头与本地验证见 [skill 插件](../plugins/skill/main.ts)、[原生解析](../crates/ply-jsc/src/skills.rs)、[CLI 参数转发](../crates/ply-cli/tests/entrypoints.rs)、[skill 集成测试](../crates/ply-kernel/tests/skills.rs)。
