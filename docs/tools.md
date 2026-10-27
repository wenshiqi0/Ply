# 工具

模型可见的工具都是普通插件：bash、read、create、edit、remove、memory、agent。它们在 `onLoad` 里把自己注册到 context，agent 再按权限收窄。

## bash 与 Ply sh

模型可见的 `bash` 工具统一使用 **Ply sh**，入参为 `command` / `cwd` / `timeout_ms`。

- 每次调用新建 shell，脚本内的状态和 cwd 独立。
- 支持内部解释器的语法、builtin、自带工具（cat、ls、grep、sed、find、sort、head、tail、wc、du 等），以及 PATH 中的宿主可执行文件；不存在的命令返回 127，无法执行返回 126。
- 默认保留启动进程 PATH 的目录顺序，再补齐系统、Homebrew 和常见用户 bin 目录；不加载 bash/zsh 配置、alias、函数或全量环境变量。
- 命令的环境是插件的 `process.env` 去掉所有以 `_API_KEY` 结尾的变量：模型密钥只供 agent 调用模型，模型执行的命令读不到。
- 内部工具和重定向使用挂载 IO；宿主程序需要磁盘 cwd，其自身的文件访问使用真实宿主文件系统，不受虚拟挂载权限隔离，参数中的虚拟绝对路径也不会自动重写。
- 超时或调用取消会回收宿主进程组。Ply sh 没有 shell 进程信号，`signal` 为 null，超时通过 `timed_out` 表示。

支持范围及缓冲管道的限制见 [ply-sh](../crates/ply-sh/README.md)。

## 执行前 hook

`bash.beforeExecute` 覆盖整个工具调用：所有监听器异步完成且未拒绝后才进入 sh，此时包含重定向和命令替换在内的脚本内容都尚未执行。

- 普通 hook 返回 `false` 或 `{ allow: false, reason }` 是硬拒绝。
- 带 `requires_approval: true` 的风险结果进入环境审批，批准后继续；缺少审批适配器时返回 `approval_required`，见 [审批与安全](approval.md)。
- `timeout_ms` 包含检查与执行时间，审批等待单独计时。

事件机制见 [RFC 0002](rfcs/0002-events.md)，hook 示例见 [examples/plugins/bash-guard](../examples/plugins/bash-guard/main.ts)。

## read 分页

读取文件优先用 `read`：

- `path` 必填；`offset` 是从 1 开始的行号（默认 1）；`limit` 是最多返回的行数（默认 200）。
- 返回原始文本 `content`、本页起始行 `offset`、返回行数 `lines`、文件总行数 `total_lines` 和 `has_more`；下一页从 `offset + lines` 开始。
- 空文件返回空文本和 0 行。分页只限制返回内容，底层仍一次读取整个文件。

create、edit、remove 同样经过挂载权限检查，只读挂载拒绝写入。
