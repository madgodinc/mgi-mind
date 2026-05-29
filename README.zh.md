# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

面向 AI 助手的自托管长期记忆。助手在工作中自己保存重要内容，之后按语义找回，不再反复问你同样的事，
也不必每次会话从零开始。一切都在你自己的机器上运行：一个 Rust 程序、本地 Qdrant 向量数据库和本地
ONNX 模型。无云端、无 API 密钥、数据不出本机。

```
你：  部署服务器地址是什么来着？

助手（调用 mind_search "部署服务器"）：
  -> "部署服务器 10.0.0.5:8080，SSH 用 deploy@，密钥在 vault 里"  (source: infra.md)

你：  好的，谢了
```

通过 MCP (Model Context Protocol) 接入助手，Claude Code 等工具会自己读写记忆。它也是一个普通 CLI，可手动使用。

> **如果你的内容主要不是英文（中文、俄文等）** - 在 `~/mgimind/config.json` 里设
> `rerank_enabled = false`。默认重排器是为英文调优的，会降低非英文的排序质量；不用它时混合搜索
> 对这些语言排得很好。详见[语言与重排器](#语言与重排器)。

---

## 目录

- [是什么](#是什么)
- [为什么用它](#为什么用它)
- [快速开始](#快速开始)
- [怎么用](#怎么用)
- [工作原理](#工作原理)
- [命令参考](#命令参考)
- [配置](#配置)
- [语言与重排器](#语言与重排器)
- [更换嵌入模型](#更换嵌入模型)
- [排错](#排错)
- [安全](#安全)
- [状态与审计](#状态与审计)
- [许可证](#许可证)

## 是什么

MGI-Mind 是你和助手之间的记忆层。助手在对话中写下简短的笔记（“记忆”）和事实，需要时把相关的取回来。
你不用打标签、不用归档、不用整理。

它为检索质量而生，而不只是存储：

- **混合搜索。** 每条记忆同时存为两个向量：稠密向量 (multilingual-e5-base) 表达语义，稀疏词频向量
  (TF-IDF，BM25 风格) 匹配精确词。查询同时走两路，两个排序列表用 Reciprocal Rank Fusion 融合。于是
  既有语义召回（“那台服务器”能找到“部署主机”），又有精确词命中（“fossilize_replay”能找到唯一用到这个
  词的笔记），一次查询完成。
- **交叉编码器重排。** 融合后的候选由交叉编码器 (bge-reranker-base) 把查询和文本一起读、重新打分，
  比只比较向量准确得多。默认开启，英文效果强（其他语言的取舍见
  [语言与重排器](#语言与重排器)）。
- **单个常驻进程。** `mgimind mcp` 本身就是 MCP 服务器：它运行整个会话，所以模型只加载一次、常驻内存，
  一次查询只要毫秒级，而不必每次调用都重新加载模型。没有守护进程、没有套接字、没有 Node 封装 -
  只有一个用 stdio 讲 MCP 的 Rust 程序。

围绕检索还有一个小型知识图谱（结构化事实）、按 agent 的会话日志（保持连续性），以及加密的、仅限终端的
密钥保险库。

## 为什么用它

没有记忆的助手会重复自己、反复要同样的上下文，也无法在昨天的工作上继续。常见的折中是你自己记笔记、打标签、
建文件夹 - 这是没完没了的杂活，而助手仍然读不好。

各替代方案都有真实短板：

- **纯笔记（Obsidian、Notion）。** 对你很好，但助手无法按语义搜索，归档还得你来。
- **裸向量数据库。** 给你语义搜索，但没有精确词匹配、没有重排、没有去重、没有会话、没有事实、没有密钥处理 -
  这些都得自己拼。
- **托管的 “memory” API。** 把你的数据发到别人的服务器。

MGI-Mind 就是拼好的本地版本：混合 + 重排检索、去重、事实、会话、保险库，都在一个你自己运行的程序里。
数据是你的，不出本机。

## 快速开始

一个程序、三步、全在终端里。无需构建工具链、无需 Node，也没有要照看的手动服务。

```bash
# 1. 从 Releases 下载适配你系统的程序（或从源码构建，见下），放到 PATH 上。
#    下面的示例假设它叫 `mgimind`。

# 2. 初始化：创建 ~/mgimind/，然后下载 Qdrant、ONNX Runtime 和模型
mgimind init
mgimind doctor --fix

# 3. 接入助手。`mgimind mcp` 本身就是 MCP 服务器；它会自己启动 Qdrant，
#    所以没有别的要运行的。
claude mcp add mgimind -- /绝对路径/mgimind mcp
```

就这样。`mgimind mcp` 运行整个会话、模型常驻；它在首次使用时拉起内置的 Qdrant。给助手看一次
[`AI_INSTRUCTIONS.md`](AI_INSTRUCTIONS.md)，让它知道流程（记录会话、回答前先搜索、密钥用 vault）。

`doctor --fix` 会下载到 `~/mgimind/`：ONNX Runtime（嵌入器加载的库）、Qdrant 程序、嵌入模型
(multilingual-e5-base，量化 ONNX，约 270 MB) 和重排模型 (bge-reranker-base，量化 ONNX，约 280 MB)。

**从 CLI 试一下**（可选 - 同一个程序也是普通命令行工具）：

```bash
mgimind create work
mgimind add work "部署服务器 10.0.0.5:8080，SSH 用 deploy@"
mgimind search "怎么连到部署服务器"
```

### 各系统注意事项

- **Linux / macOS。** 上面的步骤可直接用。在 macOS 上，首次运行下载来的程序可能需要
  `xattr -d com.apple.quarantine /path/to/mgimind`，或在 Finder 里右键 -> 打开一次。
- **Windows。** SmartScreen 可能对未签名的 `mgimind.exe` 报警（“Windows 已保护你的电脑”）-
  选择 **更多信息 -> 仍要运行**。杀毒软件也可能隔离程序或它下载的模型；如果 `mgimind doctor`
  报告某文件已下载但缺失，请在杀软里放行 `mgimind.exe` 和 `%USERPROFILE%\mgimind` 文件夹，
  然后重新运行 `mgimind doctor --fix`。（这些是终端无法点击的 GUI 提示；用代码签名消除
  SmartScreen 提示已在路线图上。）

### 从源码构建

如果你的平台没有现成发布版，或者你想自己构建，需要 Rust 工具链 (`rustup`)；没有其它依赖。

```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release                  # 程序：target/release/mgimind
```

然后运行 `target/release/mgimind init && target/release/mgimind doctor --fix`，再用
`claude mcp add mgimind -- /绝对路径/target/release/mgimind mcp` 接入。

## 怎么用

**手动（CLI）。**

```bash
mgimind add notes "妈妈生日是 3 月 14 日，喜欢芍药" --source personal
mgimind search "我妈妈的生日是哪天"
#  1. [notes] (score: 0.94) 妈妈生日是 3 月 14 日，喜欢芍药
#     source: personal

mgimind fact add "用户" "偏好" "Rust"
mgimind fact query "用户"
#   用户 -> 偏好 -> Rust

mgimind history --limit 3              # 最近三条记忆
mgimind stats                          # 按库、事实、会话的计数
```

**通过助手（MCP）。** 接好后你只管说话，工具由它自己调用：

```
你：  记住，staging 数据库密码在 vault 里，键名 "staging-db"
助手：（mind_add）已记录。密钥本身留在你的终端 vault，不在这里。

你：  staging 上用的什么数据库？
助手：（mind_search "staging 数据库"）Postgres 16，主机 db-staging.internal:5432
```

搜索按层级返回以节省 token：`--tier 1` 约 100 字符片段，`--tier 2`（默认）约 500，`--tier 3` 全文。

## 工作原理

```
  你的 AI 助手
        |  MCP (JSON-RPC over stdio)
        v
  mgimind mcp  (一个 Rust 进程：MCP 服务器 + 嵌入器，模型常驻)
        |  首次使用时启动
        v
  Qdrant（本地，仅回环）
        |
        一个 "memories" 集合，每个点两个向量：
        稠密 (e5，语义) + 稀疏 (TF-IDF，精确词)
```

**存储。** 所有记忆在一个 Qdrant 集合中。每个点的 `library` 字段区分命名空间（工作、个人、某项目），
查询可只过滤一个库或跨全部搜索。点 ID 是 `library + content` 的 UUIDv5，所以重复添加同一文本只会覆盖
同一个点：无重复、无竞态。`created_at` 的 datetime 索引让 `history` 直接取最新 N 条，而不必扫描全部。

**嵌入。** 文本用 ONNX Runtime 在本地编码。默认 multilingual-e5-base（768 维），英文强、也处理混合语言。
嵌入器可配置：池化（mean 或 CLS）、`token_type_ids` 输入、query/passage 前缀都在配置里，换模型无需改代码。
输入截断到 512 个 token，`add` 会把长文本切块，避免超出部分被静默丢弃。

**搜索。** 查询编码一次。Qdrant 在一次 Query API 调用里完成稠密最近邻和稀疏检索并用 RRF 融合。若开启重排，
前 `rerank_top_k` 个候选由交叉编码器重新打分排序。给定 `library` 过滤时，两路都生效。

**安全。** 下载按内置 SHA-256 校验，fail-closed。Qdrant 仅绑定回环，可设 API 密钥。Vault 仅限终端。
文件写入是原子的（临时文件、fsync、重命名、fsync 目录），崩溃后只会留下旧文件或新文件，不会是损坏的。

## 命令参考

### 记忆

| 命令 | 作用 |
|---|---|
| `mgimind add <库> <内容> [--source <标签>]` | 存储记忆。长文本会切块；打印块数。 |
| `mgimind search <查询> [--library <l>] [--limit N] [--tier 1\|2\|3]` | 混合搜索 + 重排。层级决定返回多少文本。 |
| `mgimind history [--limit N]` | 最近的记忆，最新在前。 |
| `mgimind delete <库> <id>` | 按 id 删除一条（id 见搜索结果）。 |
| `mgimind context` | 会话开始简报：上次会话、近期事实、库列表。 |

### 库

| 命令 | 作用 |
|---|---|
| `mgimind create <name>` | 注册一个库。 |
| `mgimind list` | 列出库。 |
| `mgimind drop <name>` | 删除一个库及其全部记忆。 |
| `mgimind stats` | 按库、事实、会话的计数及 vault 状态。 |

### 知识图谱

| 命令 | 作用 |
|---|---|
| `mgimind fact add <s> <p> <o>` | 存事实三元组。相同三元组覆盖（去重）。 |
| `mgimind fact query <词>` | 在主语/谓语/宾语里匹配词找事实。 |
| `mgimind fact invalidate <id>` | 软删除事实（留在磁盘，标记无效，查询不返回）。 |

### 会话

| 命令 | 作用 |
|---|---|
| `mgimind session start --agent <名>` | 为某 agent 开启会话日志。 |
| `mgimind session end --agent <名> --summary <文本>` | 用总结结束会话。 |
| `mgimind session last [--agent <名>]` | 显示上次会话（可指定 agent）。 |

### Vault（仅终端）

| 命令 | 作用 |
|---|---|
| `mgimind vault store <k> <v> [--category c] [--desc d]` | 存加密密钥。 |
| `mgimind vault get <k>` | 取密钥（提示主密码并确认）。 |
| `mgimind vault list` | 列出键名（从不显示值）。 |
| `mgimind vault delete <k>` | 删除密钥。 |

### 服务与数据

| 命令 | 作用 |
|---|---|
| `mgimind mcp` | 以 stdio 运行为 MCP 服务器（助手连接的就是它）。单个常驻进程；自动启动 Qdrant。 |
| `mgimind serve` / `mgimind stop` | 手动启动 / 停止内置 Qdrant（很少需要 - `mcp` 会替你做）。 |
| `mgimind migrate [--purge]` | 把旧的按库集合重新嵌入到单一 `memories` 集合。幂等。`--purge` 之后删除旧集合。 |
| `mgimind backup <file>` / `mgimind restore <file>` | 整个数据目录的 gzip+tar。 |
| `mgimind export [--format json\|md] [--output <dir>]` | 导出记忆到文件。 |
| `mgimind import <obsidian\|markdown> <path> [--library <l>]` | 导入一个 markdown 目录（递归、切块）。 |
| `mgimind doctor [--fix]` | 健康检查；`--fix` 下载缺失项。 |

## 配置

配置在 `~/mgimind/config.json`。影响检索的字段：

| 字段 | 默认 | 含义 |
|---|---|---|
| `model_name` | `multilingual-e5-base` | `models/` 下的模型目录。 |
| `vector_size` | `768` | 嵌入维度（须与模型一致）。 |
| `pooling` | `mean` | `mean`（e5、MiniLM）或 `cls`（部分 XLM-R）。 |
| `uses_token_type_ids` | `false` | BERT 系为 `true`，XLM-R / e5 为 `false`。 |
| `query_prefix` / `passage_prefix` | `query: ` / `passage: ` | e5 需要，其它模型留空。 |
| `rerank_enabled` | `true` | 交叉编码器重排。见语言一节。 |
| `rerank_model` | `bge-reranker-base` | `models/` 下的重排模型目录。 |
| `rerank_top_k` | `20` | 返回 `limit` 前取出并重排的候选数。 |
| `qdrant_port` | `6334` | Qdrant gRPC 端口。 |
| `qdrant_api_key` | 无 | 设置后 Qdrant 带它启动，客户端做鉴权。 |

## 语言与重排器

默认这套是为英文调优的 - 因为助手本身在英文上推理最好，模型在英文上也最强。英文优先的项目推荐这套。

几个实话：

- 嵌入器 (multilingual-e5-base) 确实是多语言的。英文查询能找到中文/俄文笔记，反之亦然；单靠搜索跨语言效果就不错。
- 默认重排器 (bge-reranker-base) 是英文调优的。它提升英文排序，但会**降低非英文（中文、俄文等）的排序质量**。
  这是默认设置唯一偏向英文的地方。
- **如果你的内容主要是非英文：** 设 `rerank_enabled = false`。混合稠密+稀疏搜索本身对这些语言排得很好；
  拖后腿的是英文调优的重排器。或者换一个更强的多语言重排器。
- 重排每次查询要做交叉编码器推理：纯 CPU 上约 1-2 秒处理 20 个候选。需要更快就调小 `rerank_top_k` 或关闭重排。

## 更换嵌入模型

换模型通常会改变向量维度，所以已有记忆要重新嵌入：

1. 先备份：`mgimind backup ~/mgi-backup.tar.gz`。
2. 在 `config.json` 里为新模型设 `model_name`、`vector_size`、`pooling`、`uses_token_type_ids` 和前缀。
3. `mgimind doctor --fix` 下载它。（只有内置默认项被固定校验；自定义模型下载时会有 “integrity not verified”
   警告 - 在意的话把它的 SHA-256 固定到 `integrity.rs`。）
4. `mgimind migrate` 用新模型从已存文本重新嵌入全部。

## 排错

- **“Model not found ... run doctor --fix”。** 模型不在 `~/mgimind/models/`。运行 `mgimind doctor --fix`。
- **“invalid expand shape” / 推理报错。** 通常是输入远超 512 token。`add` 会自动切块；直接调库时请先切块。
- **搜索慢。** 那是 CPU 上的重排。调小 `rerank_top_k`，或设 `rerank_enabled = false`。（模型会在
  `mgimind mcp` 进程的整个生命周期内自动常驻，所以只有一个会话里的第一次查询要付加载代价。）
- **装完后某个工具就报错。** 运行 `mgimind doctor`（助手可调用 `mind_doctor`）：它会准确报告缺什么 -
  Qdrant 没运行、某个模型没下载、缺 ONNX Runtime，或文件被杀软隔离 - `--fix` 会下载它能下的。
- **维度不匹配警告。** 集合向量与 `vector_size` 不符，通常发生在换模型后。用 `mgimind migrate` 重新嵌入。
- **中文/非英文结果不准。** 设 `rerank_enabled = false`（见语言一节）。

## 安全

- 下载按内置 SHA-256 校验（fail-closed）：ONNX Runtime (linux-x64)、Qdrant 和默认模型（e5 和重排器）。
  其它平台和自定义模型会警告，而非盲目信任。
- Qdrant 仅绑定 `127.0.0.1`，支持 API 密钥。
- Vault 使用 AES-256-GCM，密钥由 Argon2id 派生（参数固定，库升级不会把你锁在外面）。仅限终端：主密码和
  解密后的密钥从不经 MCP 通道。`mind_vault_*` 这组 MCP 工具只返回终端操作说明，从不返回密钥值。
- 文件写入是原子的并 fsync 目录 - 崩溃后留下旧文件或新文件，而非损坏文件。

## 状态与审计

当前版本：**0.8.x**。项目完成了一次完整代码审计（27 个问题）：**21 个完全修复，6 个部分修复**
（机制已落地，仍在加固）。[`AUDIT_STATUS.md`](AUDIT_STATUS.md) 逐项交代，包括诚实的缺口
（例如事实 supersession 尚未实现）。[`CHANGELOG.md`](CHANGELOG.md) 是各版本历史。

## 许可证

Apache-2.0。见 [`LICENSE`](LICENSE)。
