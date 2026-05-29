# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

面向 AI 助手的自托管长期记忆。助手在会话中保存重要内容，之后按语义找回，不必每次从零开始。
一切都在你自己的机器上运行：一个 Rust 程序、本地 Qdrant 向量数据库和本地 ONNX 模型。
无云端、无 API 密钥、数据不出本机。

```
"部署服务器地址是什么？"

  mgimind search "部署服务器地址"
  -> 部署服务器 10.0.0.5:8080   (source: infra.md)
```

通过 MCP (Model Context Protocol) 连接助手，Claude Code 等工具可直接读写记忆。也可作为普通 CLI 使用。

## 状态

当前版本：**0.7.x**。项目完成了一次完整的安全与质量审计（27 个问题），现已全部处理。
逐项核对见 [`AUDIT_STATUS.md`](AUDIT_STATUS.md)，各版本历史见 [`CHANGELOG.md`](CHANGELOG.md)。

核心是检索：

- **混合搜索。** 每条记忆存为两个向量：稠密向量 (multilingual-e5-base) 表示语义，
  稀疏 BM25 向量匹配精确词。查询同时走两路，用 Reciprocal Rank Fusion 融合结果，
  既有语义召回也有关键词精度。
- **交叉编码器重排。** 融合后的候选由交叉编码器 (bge-reranker-base) 把查询和文本一起读、重新打分。
  默认开启，英文效果强。
- **常驻守护进程。** 长期运行的进程让模型保持加载，通过 Unix 套接字服务 MCP 客户端，
  这样每次查询不必再支付模型加载时间。

## 工作原理

- **存储。** 所有记忆在一个 Qdrant 集合中。`library` 字段区分命名空间，可在需要时过滤。
  点 ID 是 `library + content` 的 UUIDv5，因此重复添加同一文本是幂等覆盖，不产生重复。
- **嵌入。** 用 ONNX Runtime 在本地编码。默认模型 multilingual-e5-base（768 维），
  英文强且支持混合语言。嵌入器可配置（池化、前缀、token_type_ids），换模型无需改代码。
  输入截断到 512 个 token。
- **搜索。** 查询编码一次，Qdrant 在一次 Query API 调用中完成稠密和稀疏检索并用 RRF 融合。
  若开启重排，前 `rerank_top_k` 个候选由交叉编码器重新排序。`library` 过滤同时作用于两路。
- **安全。** 下载按 SHA-256 校验（有锁定哈希时）。Qdrant 仅绑定本地回环，可设 API 密钥。
  密钥保险库仅限终端：主密码无回显输入、用后清零、绝不经 MCP 通道返回。文件写入是原子的。

## 安装 (Linux)

需要：Rust 工具链 (`rustup`)，以及用于 MCP 服务器的 Node 或 Bun。

```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release            # 程序：target/release/mgimind

target/release/mgimind init
target/release/mgimind doctor --fix   # 下载 ONNX Runtime、Qdrant 和模型
target/release/mgimind serve          # 启动 Qdrant
target/release/mgimind doctor         # 检查
```

`doctor --fix` 会下载到 `~/mgimind/`：ONNX Runtime、Qdrant 程序、嵌入模型
(multilingual-e5-base，量化 ONNX，约 270 MB) 和重排模型 (bge-reranker-base，约 280 MB)。
若 ONNX Runtime 不在程序旁边，用 `ORT_DYLIB_PATH` 指定路径。macOS 和 Windows 同样用
`cargo build --release` 构建。

### MCP 服务器

```bash
cd mcp-server
bun install        # 或：npm install
```

接入 Claude Code：

```bash
claude mcp add mgi-mind -- node /绝对路径/mgi-mind/mcp-server/index.js
```

MCP 服务器优先连接常驻守护进程 (`mgimind daemon`)，若未运行则回退到启动 CLI，两种方式都能用。

## 命令

```
mgimind add <lib> "文本" [--source 标签]    存储记忆
mgimind search "查询" [--library l] [--limit N] [--tier 1|2|3]   混合搜索 + 重排
mgimind history [--limit N]                 最近的记忆
mgimind delete <lib> <id>                   按 id 删除一条
mgimind context                             会话开始简报

mgimind create <lib> / drop <lib> / list    库管理
mgimind stats                               统计

mgimind fact add S P O / query <词> / invalidate <id>   知识图谱事实

mgimind session start --agent <名> / end --agent <名> --summary "总结" / last   会话

mgimind vault store K V / get K / list / delete K   密钥（仅终端）

mgimind serve / stop                        启动/停止 Qdrant
mgimind daemon                              常驻守护进程
mgimind migrate [--purge]                   把旧集合迁移到新集合
mgimind backup <file> / restore <file>      备份/恢复
mgimind export [--format json|md]           导出
mgimind import obsidian /path               导入 markdown
mgimind doctor [--fix]                      检查/自动安装
```

## 配置

配置在 `~/mgimind/config.json`。检索相关字段：

| 字段 | 默认 | 含义 |
|---|---|---|
| `model_name` | `multilingual-e5-base` | `models/` 下的模型目录 |
| `vector_size` | `768` | 嵌入维度（须与模型一致） |
| `pooling` | `mean` | `mean` (e5, MiniLM) 或 `cls` (部分 XLM-R) |
| `uses_token_type_ids` | `false` | BERT 系为 true，XLM-R/e5 为 false |
| `query_prefix` / `passage_prefix` | `query: ` / `passage: ` | e5 需要，其它模型留空 |
| `rerank_enabled` | `true` | 交叉编码器重排（英文强） |
| `rerank_model` | `bge-reranker-base` | `models/` 下的重排模型目录 |
| `rerank_top_k` | `20` | 取出并重排的候选数 |

### 关于模型与语言

- 默认嵌入器是多语言的：英文查询能找到其它语言的内容，反之亦然。
- 默认重排器 (bge-reranker-base) 针对英文，能提升英文排序，但会降低俄文排序。
  俄文为主时可设 `rerank_enabled=false`（稠密+稀疏混合本身对俄文排序就不错），或换更强的多语言重排器。
- 重排会为每次查询增加推理。在纯 CPU 上约 1-2 秒处理 20 个候选。需要更低延迟就调小 `rerank_top_k` 或关闭重排。

### 更换嵌入模型

换模型通常会改变向量维度，因此已有记忆需要重新嵌入。为新模型设置 `model_name`、`vector_size`、
`pooling`、`uses_token_type_ids` 和前缀，运行 `mgimind doctor --fix` 下载，再用 `mgimind migrate`
从已存文本重新嵌入。请先备份。

## 许可证

Apache-2.0 - [Mad God Inc](https://github.com/madgodinc), 2026
