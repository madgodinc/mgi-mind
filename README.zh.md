# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

AI原生第二大脑。自托管、令牌优化的AI助手记忆系统。

用AI自动记忆一切，替代Obsidian/Notion。

```
"服务器地址是什么？"

-> mgimind search "服务器地址"
-> 部署服务器 10.0.0.5:8080 (score: 0.72)
```

> **v0.2.0** — 根据完整代码审计重建了数据与安全层：原子写入、内容寻址 ID（幂等 upsert）、
> 隐藏并清零的保险库主密码、下载 SHA-256 校验、Qdrant 仅绑定 127.0.0.1、按 agent 的会话、
> 原生 HTTP/归档处理、测试 + CI。完整核对见 [`AUDIT_STATUS.md`](AUDIT_STATUS.md)，
> 变更见 [`CHANGELOG.md`](CHANGELOG.md)。

完整文档和代码示例请参阅[英文README](README.md)。

---

## 快速开始

```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release

mgimind init
mgimind doctor --fix   # 下载Qdrant、ONNX Runtime、嵌入模型
mgimind serve          # 启动向量数据库

mgimind create work
mgimind add work "部署服务器10.0.0.5，端口8080"
mgimind search "服务器地址"
```

## MCP集成

```json
{
  "mcpServers": {
    "mgi-mind": {
      "command": "bun",
      "args": ["run", "/path/to/mgi-mind/mcp-server/index.js"]
    },
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

## 功能特性

- **语义搜索** - 按含义搜索，而非关键词
- **知识图谱** - 结构化事实（主语 -> 谓语 -> 宾语）
- **会话日志** - AI记住上次的工作内容
- **加密保险库** - 密码、SSH、API密钥（AES-256-GCM + Argon2）
- **网页阅读器** - AI通过CRW（Rust）读取任何网页
- **Obsidian导入** - 从现有知识库导入.md文件
- **导出** - JSON和Markdown格式
- **去重** - 不会重复存储相同内容
- **自配置** - AI建议将规则写入自己的配置文件
- **分层检索** - 节省10-20倍令牌
- **跨平台** - Windows、macOS（Intel + Apple Silicon）、Linux

## 所有命令

```
mgimind init                - 初始化
mgimind doctor --fix        - 自动下载依赖
mgimind serve / stop        - 启动/停止Qdrant

mgimind create <lib>        - 创建库
mgimind drop <lib>          - 删除库
mgimind list                - 列出库
mgimind add <lib> "text"    - 添加记忆
mgimind search "query"      - 语义搜索 (--tier 1/2/3)
mgimind delete <lib> <id>   - 删除特定记忆

mgimind fact add S P O      - 添加知识图谱事实
mgimind fact query S        - 查询事实
mgimind fact invalidate <id> - 删除事实

mgimind session start       - 开始会话日志
mgimind session last        - 读取上次会话
mgimind session end         - 结束并写入总结

mgimind vault store K V     - 存储密钥（AES-256-GCM加密）
mgimind vault get K         - 获取（需主密码）
mgimind vault list          - 列出密钥名（不显示值）
mgimind vault delete K      - 删除密钥

mgimind context             - AI会话简报
mgimind history             - 最近添加的记忆
mgimind stats               - 统计信息
mgimind web <url>           - 读取网页（通过CRW）
mgimind web <url> --save X  - 读取并保存到库
mgimind import obsidian /path - 从Obsidian导入
mgimind export --format json  - 导出数据

mgimind backup <file>       - 备份
mgimind restore <file>      - 恢复
```

## 技术栈

| 组件 | 技术 |
|------|------|
| 核心引擎 | Rust |
| 向量数据库 | Qdrant |
| 嵌入 | ONNX Runtime (all-MiniLM-L6-v2) |
| MCP服务器 | Bun |
| 网页阅读器 | CRW (Rust) |
| 加密 | AES-256-GCM + Argon2 |
| 许可证 | Apache 2.0 |

## 许可证

Apache 2.0 - [Mad God Inc](https://github.com/madgodinc), 2026
