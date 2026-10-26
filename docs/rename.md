# Ply 改名说明

项目和命令统一使用 ply，不保留旧运行时入口。

| 原名称 | 新名称 |
|---|---|
| wendy | ply |
| wendy:* 模块 | ply:* 模块 |
| WENDY_* 环境变量 | PLY_* 环境变量 |
| wendy-* Rust 包 | ply-* Rust 包 |
| ~/.local/state/wendy/checkpoints | ~/.local/state/ply/checkpoints |

例如：

```ts
import { context, agent } from "ply:plugins";
```

```bash
ply tui
ply tui --bypass
ply tui --plugin-options 'skill={"paths":["./examples/skills"]}'
cargo install --path crates/ply-cli --debug --force --locked --offline
```

第三方插件需要更新模块前缀。供应商密钥名 DEEPSEEK_API_KEY、MIMO_API_KEY、TYPESAFE_API_KEY 保持供应商定义；项目自己的配置只读取 PLY_*，不回退读取旧名称。

旧状态文件原地保留，不移动、改写或自动迁移；新调用使用新目录。项目的旧私有 .wendy 目录继续被 Git 忽略，防止旧数据意外入库。恢复记录格式为 4，context 快照格式为 2，旧格式明确拒绝。

仓库和托管工作区的根路径不随产品名搬迁；Rust 包目录已经统一为 crates/ply-*。Git 远端不在工具改名中变更。
