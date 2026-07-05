# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

**[最新版本：v2.3.0](https://github.com/madgodinc/mgi-mind/releases/tag/v2.3.0)** · **[CHANGELOG](CHANGELOG.md)** · **[Discussions](https://github.com/madgodinc/mgi-mind/discussions)** · **[Issues](https://github.com/madgodinc/mgi-mind/issues)** · **[Contributing](CONTRIBUTING.md)**

面向 AI 助手的本地长期记忆。一个 Rust 程序，本地 Qdrant 向量数据库，本地
ONNX 模型。通过 MCP 协议，Claude Code 等助手可以自己读写记忆。同时也是一个普通的
CLI 工具。

```
你：  部署服务器地址是什么来着？

助手（调用 mind_search "部署服务器"）：
  -> "部署服务器 10.0.0.5:8080，SSH 用 deploy@，密钥在 vault 里"  (source: infra.md)

你：  好的，谢了
```

数据不离开本机 — 嵌入、检索、重排、vault 全部本地运行。无云端账号、无 API
密钥、无遥测。

---

## 目录

- [它是什么](#它是什么)
- [为什么用它](#为什么用它)
- [快速开始](#快速开始)
- [使用方式](#使用方式)
- [工作原理](#工作原理)
- [命令参考](#命令参考)
- [配置](#配置)
- [语言与重排器](#语言与重排器)
- [更换嵌入模型](#更换嵌入模型)
- [故障排查](#故障排查)
- [安全](#安全)
- [状态与审计](#状态与审计)
- [项目结构](#项目结构)
- [许可证](#许可证)

## 它是什么

MGI-Mind 位于你与助手之间。助手在对话中写下简短笔记（"记忆"）和事实，并在需要时
按语义把相关内容取回来。

侧重检索，不只是存储：

- **混合检索。** 每条记忆同时以两种向量存储：稠密向量
  (multilingual-e5-base, 768 维) 表达语义，稀疏 TF-IDF（BM25 风格）向量
  表达精确词项。一次查询同时跑两路，并用 Reciprocal Rank Fusion 合并。
  "服务器盒子" 通过稠密路找到 "deploy host"；`fossilize_replay`
  通过稀疏路找到唯一包含该词项的笔记。
- **交叉编码器重排。** 融合后的候选会被 bge-reranker-base 重新评分，
  它把查询与段落一起读，比单纯比较向量更准。默认开启，英文优化 — 关于其他
  语言上的权衡，见 [语言与重排器](#语言与重排器)。
- **一个常驻进程。** `mgimind mcp` 本身就是 MCP 服务器：整个会话期间保持
  运行，模型加载一次后常驻内存，一次查询是毫秒级而不是每次重新加载模型。

围绕检索还有一个用于结构化事实的知识图谱、按助手区分的会话日志（用于跨会话
连续性）、以及只在终端使用的加密 vault（用于秘密）。

## 为什么用它

没有记忆的助手每次会话都需要重新给上下文，无法在昨天的工作之上继续。常见的
变通方式是你自己维护笔记、标签和文件夹——但助手仍然无法按语义读它们。

MGI-Mind 与 Obsidian、Notion 的根本区别：**系统决定写下什么，不是你。**
MCP 服务器实时读助手在做什么，并通过相关性门把事实、决策、修复路由进存储。
你不归档，不打标签，不决定"什么值得保存"。低信号候选会落到隔离区
（再次被重申时可恢复），而不会污染检索。

与显而易见的替代方案的对比：

- **普通笔记（Obsidian, Notion）。** 作为个人笔记本很强，但助手无法按
  语义检索，而且对一堆 `.md` 文件来说每次按键都"同等重要"。
- **裸向量数据库。** 给你语义检索，但没有精确词项匹配、没有重排、没有
  去重、没有会话、没有事实、没有秘密处理、没有相关性门、没有程序性记忆。
  这些都要你自己组装。
- **托管的"记忆"API。** 你的数据在别人的服务器上。同时关闭了对去重行为、
  相关性门实际在做什么的可检查性。

MGI-Mind 是组装好的本地版本：混合 + 重排检索、相关性门、去重、事实、会话、
程序性记忆（"错误 → 修复"剧本）、终端 vault — 全部封装在你自己运行的一个
程序里。

## 快速开始

一条命令。安装器把程序放到 PATH，并运行 `init` + `doctor --fix`
（后者会拉取 Qdrant、ONNX Runtime 和模型）。

**Linux / macOS：**

```bash
curl -fsSL https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.sh | sh
```

**Windows**（PowerShell）：

```powershell
irm https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.ps1 | iex
```

安装完成后，会打印出把服务器接入 Claude Code 的确切命令：

```bash
claude mcp add mgimind -- /home/you/.local/bin/mgimind mcp
```

`mgimind mcp` 本身就是 MCP 服务器；它在整个会话期间运行，模型保持热加载，
首次使用时会拉起内置的 Qdrant。把 [`AI_INSTRUCTIONS.md`](AI_INSTRUCTIONS.md)
给助手看一次，让它了解协议（记录会话、回答前先检索、秘密用 vault）。

`doctor --fix` 把以下内容下载到 `~/mgimind/`：ONNX Runtime（嵌入器加载的
库）、Qdrant 程序、嵌入模型（multilingual-e5-base，量化 ONNX，约 270 MB）
以及重排器（bge-reranker-base，量化 ONNX，约 280 MB）。

### 安装器选项

- `INSTALL_DIR=/opt/mgimind curl ... | sh` — 装到非 `~/.local/bin` 的位置。
- `MGIMIND_TAG=v2.3.0 curl ... | sh` — 锁定具体版本，而不是 `latest`。
- `SKIP_DOCTOR=1 curl ... | sh` — 只放下程序；之后自己运行 `init` + `doctor --fix`。

### 手动安装（不用脚本）

如果不想把脚本管道给 shell，从 [Releases](https://github.com/madgodinc/mgi-mind/releases/latest)
下载对应平台的压缩包，把 `mgimind` 放到 PATH，然后：

```bash
mgimind init
mgimind doctor --fix
claude mcp add mgimind -- /absolute/path/to/mgimind mcp
```

从 CLI 试用（同一个程序也是普通命令行工具）：

```bash
mgimind create work
mgimind add work "部署服务器 10.0.0.5:8080，SSH 用 deploy@"
mgimind search "如何连接部署服务器"
```

### 平台说明

- **Linux x86_64、macOS arm64（Apple Silicon）、Windows x86_64** —
  每个版本都有预编译程序。安装器会自动选对。
- **macOS Intel (x86_64)** — 没有预编译程序。GitHub 托管的 `macos-13`
  runner 排队 20-30+ 分钟且在被逐步淘汰，所以不在发布矩阵中。从源码构建
  （见下一节）；只需几分钟。
- **macOS 首次运行检疫** — 下载的程序可能需要
  `xattr -d com.apple.quarantine /path/to/mgimind`，或在 Finder 中右键 → 打开 一次。
- **Windows** — SmartScreen 可能对未签名的 `mgimind.exe` 发出警告
  （"Windows protected your PC"）— 选择 **More info → Run anyway**。
  杀毒软件也可能隔离程序或它下载的模型；如果 `mgimind doctor` 报告某文件
  已下载但缺失，在 AV 中允许 `mgimind.exe` 和 `%USERPROFILE%\mgimind`
  文件夹，然后重跑 `mgimind doctor --fix`。代码签名以去掉 SmartScreen
  提示在路线图中。

### 从源码构建

需要 Rust 工具链（`rustup`）；无其他依赖。

```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release                  # 程序：target/release/mgimind
```

然后运行 `target/release/mgimind init && target/release/mgimind doctor --fix`
并用 `claude mcp add mgimind -- /absolute/path/to/target/release/mgimind mcp`
接入。

## 使用方式

**从 CLI：**

```bash
mgimind add notes "妈妈生日 3 月 14 号，她喜欢牡丹" --source personal
mgimind search "妈妈什么时候生日"
#  1. [notes] (score: 0.94) 妈妈生日 3 月 14 号，她喜欢牡丹
#     source: personal

mgimind fact add "user" "prefers" "Rust"
mgimind fact query "user"
#   user -> prefers -> Rust

mgimind history --limit 3              # 最近的三条记忆
mgimind stats                          # 各库、事实、会话的计数
```

**通过助手（MCP）。** 接入后，直接对话即可。助手会自己调用工具：

```
你：  记一下，staging 数据库密码在 vault 里，键叫 "staging-db"
助手：(mind_add) 已保存。秘密本身留在你的终端 vault 里，不在这里。

你：  staging 上用的什么数据库？
助手：(mind_search "staging 数据库") Postgres 16，主机 db-staging.internal:5432
```

检索按层级返回结果，让助手谨慎使用 token：`--tier 1` 是约 100 字符的片段，
`--tier 2`（默认）约 500 字符，`--tier 3` 是全文。

## 工作原理

```
  你的 AI 助手
        |  MCP（基于 stdio 的 JSON-RPC）
        v
  mgimind mcp（一个 Rust 进程：MCP 服务器 + 嵌入器，模型常驻）
        |  首次使用时启动
        v
  Qdrant（本地，仅 loopback）
        |
        一个 "memories" 集合，每个点两种向量：
        稠密（e5，语义）+ 稀疏（TF-IDF，精确词项）
```

**存储。** 所有记忆都在一个 Qdrant 集合里。每个点上的 `library` 字段分隔
命名空间（work、personal、某个项目），查询可以过滤到一个库，也可以跨所有
库。点的 ID 是 `library + content` 的 UUIDv5，所以同样的文本添加两次只是
覆盖同一个点——没有重复、没有竞态。`created_at` 的 datetime 索引让
`history` 直接返回最新 N 条，不必扫描全部。

**嵌入。** 文本通过 ONNX Runtime 在本地嵌入。默认是 multilingual-e5-base
（768 维），英文强，也能处理混合语言。嵌入器模型敏感：池化方式（mean
或 CLS）、`token_type_ids` 输入、query/passage 前缀——都在配置里，
换模型不需要改代码。输入限制 512 个 token；`add` 会把长文本切分，
不会让超出上限的部分静默丢失。

**检索。** 查询嵌入一次。Qdrant 在一次 Query API 调用里同时跑稠密 ANN
检索和稀疏检索，并用 RRF 融合。如果开了重排，前 `rerank_top_k` 个候选
会被交叉编码器重新评分并重新排序。如果给了 `library` 过滤器，会作用于
两路。

**安全。** 下载会对照 pinned SHA-256 校验（fail-closed）。Qdrant 只绑定
到 loopback，可以要求 API key。Vault 只在终端使用——主密码与解密后的
秘密永远不会经过 MCP 通道。文件写入是原子的（临时文件、fsync、rename、
fsync 目录），所以崩溃后留下的要么是旧文件，要么是新文件，绝不会是
损坏的文件。

## 命令参考

### 记忆

| 命令 | 作用 |
|---|---|
| `mgimind add <library> <content> [--source <tag>]` | 存一条记忆。长文本会被切分；打印存了多少块。 |
| `mgimind search <query> [--library <l>] [--limit N] [--tier 1\|2\|3]` | 混合检索后重排。Tier 决定返回多少文本。 |
| `mgimind history [--limit N]` | 最新的记忆，新的在前。 |
| `mgimind delete <library> <id>` | 按 id 删一条记忆（id 在检索结果里显示）。 |
| `mgimind context` | 紧凑的会话开始简报：上次会话、近期事实、各库。 |

### 库

| 命令 | 作用 |
|---|---|
| `mgimind create <name>` | 注册一个库。 |
| `mgimind list` | 列出库。 |
| `mgimind drop <name>` | 删除一个库及其所有记忆。 |
| `mgimind stats` | 各库、事实、会话的计数，以及 vault 状态。 |

### 知识图谱

| 命令 | 作用 |
|---|---|
| `mgimind fact add <subject> <predicate> <object>` | 存一条事实三元组。相同三元组会覆盖（去重）。 |
| `mgimind fact query <term>` | 查找 term 出现在 subject、predicate 或 object 中的事实。 |
| `mgimind fact invalidate <id>` | 软删除一条事实（保留在磁盘上，标记为无效，查询时隐藏）。 |

### 会话

| 命令 | 作用 |
|---|---|
| `mgimind session start --agent <name>` | 开始一个助手的会话日志。 |
| `mgimind session end --agent <name> --summary <text>` | 用一段摘要结束会话。 |
| `mgimind session last [--agent <name>]` | 显示上一次会话（可选只看某个助手）。 |

### Vault（仅终端）

| 命令 | 作用 |
|---|---|
| `mgimind vault store <key> <value> [--category c] [--desc d]` | 存一个加密秘密。 |
| `mgimind vault get <key>` | 取一个秘密（会提示主密码，然后再确认一次）。 |
| `mgimind vault list` | 列出键名（永远不显示值）。 |
| `mgimind vault delete <key>` | 删一个秘密。 |

### 服务与数据

| 命令 | 作用 |
|---|---|
| `mgimind mcp` | 以 stdio 模式运行 MCP 服务器（助手连接的就是这个）。一个常驻进程；自动启动 Qdrant。 |
| `mgimind serve` / `mgimind stop` | 手动启动 / 停止内置 Qdrant（一般不用——`mcp` 会自己处理）。 |
| `mgimind migrate [--purge]` | 把老的按库分开的集合重新嵌入到统一的 `memories` 集合。幂等。`--purge` 会在之后删除老集合。 |
| `mgimind backup <file>` / `mgimind restore <file>` | 整个数据目录的 gzip+tar。 |
| `mgimind export [--format json\|md] [--output <dir>]` | 把记忆导出到文件。 |
| `mgimind import <obsidian\|markdown> <path> [--library <l>]` | 导入一个 markdown 文件夹（递归，会切分）。 |
| `mgimind doctor [--fix]` | 健康检查；`--fix` 会下载缺失的内容。 |

## 配置

配置在 `~/mgimind/config.json`。影响检索的字段：

| 字段 | 默认 | 含义 |
|---|---|---|
| `model_name` | `multilingual-e5-base` | `models/` 下的嵌入模型目录。 |
| `vector_size` | `768` | 嵌入维度。必须与模型一致。 |
| `pooling` | `mean` | `mean`（e5, MiniLM）或 `cls`（部分 XLM-R 模型）。 |
| `uses_token_type_ids` | `false` | BERT 家族模型为 `true`，XLM-R / e5 为 `false`。 |
| `query_prefix` / `passage_prefix` | `query: ` / `passage: ` | e5 需要这些；不需要的模型留空。 |
| `rerank_enabled` | `true` | 交叉编码器重排。见下方语言说明。 |
| `rerank_model` | `bge-reranker-base` | `models/` 下的重排器目录。 |
| `rerank_top_k` | `20` | 在返回 `limit` 之前取并重排多少候选。 |
| `qdrant_port` | `6334` | Qdrant gRPC 端口。 |
| `qdrant_api_key` | 无 | 如果设置，Qdrant 会以此启动，客户端用它认证。 |

## 语言与重排器

默认栈为英文优化，因为助手自身在英文上推理最强，模型也最强。这对英文优先
的项目是推荐的设置。

几个诚实的细节：

- 嵌入器（multilingual-e5-base）确实是多语言的。英文查询能找到中文笔记，
  反之亦然，单凭检索在跨语言上工作得很好。
- 默认重排器（bge-reranker-base）是英文调优的。它能改善英文排序，但
  **会降低中文排序质量**（其他非英语语言同理）。这是默认设置唯一偏向
  英文的地方。
- **如果你的内容主要是中文（或其他非英文语言）：** 设
  `rerank_enabled = false`。混合稠密+稀疏检索本身就能很好地排序这些
  语言；正是英文调优的重排器在拖累它们。或者换一个更强的多语言重排器。
- 重排每个查询都要做交叉编码器推理——20 个候选在纯 CPU 机器上大约
  1-2 秒。降低 `rerank_top_k` 或关掉重排可让检索更快。

## 更换嵌入模型

换模型通常会改变向量维度，所以现有记忆必须重新嵌入：

1. 先备份：`mgimind backup ~/mgi-backup.tar.gz`。
2. 在 `config.json` 里为新模型设置 `model_name`、`vector_size`、
   `pooling`、`uses_token_type_ids` 和前缀。
3. `mgimind doctor --fix` 下载它。只有自带默认模型有 pinned；自定义模型
   下载时会有 "integrity not verified" 警告，如果想严格校验，把它的
   SHA-256 写到 `integrity.rs` 里。
4. `mgimind migrate` 用新模型基于已存文本重新嵌入。

## 故障排查

- **"Model not found ... run doctor --fix"** — `~/mgimind/models/`
  里没有那个模型。运行 `mgimind doctor --fix`。
- **"invalid expand shape" / 推理错误** — 通常是输入远超 512 tokens。
  `add` 会自动切分；如果直接调库，请先切分。
- **检索慢** — 是 CPU 上的重排器。降低 `rerank_top_k`，或设
  `rerank_enabled = false`。模型在 `mgimind mcp` 进程的整个生命周期
  里保持热加载，所以一次会话只有第一次查询要付加载成本。
- **某个工具刚装好就报错** — 运行 `mgimind doctor`（助手可以调用
  `mind_doctor`）；它会精确报告缺什么（Qdrant 没跑、某模型没下载、
  ONNX Runtime 缺失、文件被 AV 隔离），`--fix` 能下载的都会下载。
- **维度不匹配警告** — 集合里的向量与 `vector_size` 不一致，通常是
  换过模型。用 `mgimind migrate` 重新嵌入。
- **中文结果感觉怪** — 设 `rerank_enabled = false`（见上面的语言说明）。

## 安全

- 对 ONNX Runtime (linux-x64)、Qdrant、默认模型 (e5 和重排器)，下载
  会对照 pinned SHA-256 校验（fail-closed）。其他平台和自定义模型会
  警告而不是盲目信任。
- Qdrant 只绑定到 `127.0.0.1`，支持 API key。
- Vault 用 AES-256-GCM，密钥由 Argon2id 派生（参数 pinned，库升级
  不会把你锁在外面）。只在终端使用——主密码与解密后的秘密永远不会
  经过 MCP 通道。MCP 工具 `mind_vault_*` 返回的是终端使用说明，不是
  秘密值本身。
- 文件写入是原子的，目录会 fsync——崩溃后留下的要么是旧文件，要么
  是新文件，绝不是损坏的文件。

## 状态与审计

当前版本：**2.3.0**（自 v1.0.0 起 semver 稳定）。基于 0.10.x 的审计
日志和临时 viewer、0.11.x 的隔离层 + best-effort retrieval 策略、
0.12.x 的 viewer 波次、0.13.x 的 session liveness、以及 0.14.x 的
procedural-memory 护城河（LongMemEval baseline + 来自 20 个公开仓库的
227 对 Д6 数据集）构建。1.0 合约 — md reconcile 的非对称
「Qdrant 现在 → md 将成为」diff、`MGIMIND_MODEL_VARIANT={cpu|gpu|auto}`
开关、以及 31 工具的 MCP 表面 — 在 2.0 之前冻结。

项目经过 27 个问题的代码审计。**21 个完全修复，6 个部分修复** —
机制已上线，加固在继续。[`AUDIT_STATUS.md`](AUDIT_STATUS.md) 逐条
说明每个问题，包括坦诚的差距（比如 fact supersession 还没实现）。
[`CHANGELOG.md`](CHANGELOG.md) 是按版本的历史记录；
[`ROADMAP.md`](ROADMAP.md) 说明 v1.1–v2.0 的已承诺范围，以及在
v3.0 视野上**仍是候选方向**的事项。

## 项目结构

```
src/
  cli.rs         命令分派与输出渲染
  storage.rs     Qdrant：单集合、混合检索、history、stats、migrate、切分
  embedder.rs    ONNX 嵌入（模型敏感的池化、前缀、512 token 上限）
  reranker.rs    交叉编码器重排
  knowledge.rs   知识图谱事实
  procedure.rs   程序性记忆：learn / recall / outcome
  ingest.rs      自动提取并接入候选
  relevance.rs   v0.11 相关性门（长度、黑名单、决策标记、token 新颖度）
  consolidate.rs 合并重复、报告冷条目
  md_reconcile.rs  以 "md wins" 策略的 md 导入
  audit.rs       每次存储变更的 append-only 审计日志
  viewer.rs      临时本地 HTTP viewer（axum，静态前端内嵌）
  session.rs     按助手的会话文件
  vault.rs       加密秘密 vault（仅终端）
  mcp.rs         stdio 上的 MCP 服务器（手写 JSON-RPC；进程内常驻模型）
  config.rs      配置
  integrity.rs   下载的 pinned SHA-256 哈希
  util.rs        原子写入、校验过的下载
tests/
  cli_integration.rs   对真实 Qdrant 的黑盒测试（CLI + MCP 往返）
```

## 许可证

Apache-2.0。见 [`LICENSE`](LICENSE)。
