# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

Self-hosted long-term memory for AI assistants. Your assistant stores what matters
during a session and finds it later by meaning, so it does not start from zero every
time. Everything runs on your own machine: a Rust binary, a local Qdrant vector
database, and local ONNX models. No cloud, no API keys, no data leaving the box.

```
"What was the deploy server address?"

  mgimind search "deploy server address"
  -> Deploy server is 10.0.0.5:8080   (source: infra.md)
```

It connects to your assistant over MCP (Model Context Protocol), so tools like
Claude Code can read and write memory directly. It also works as a plain CLI.

## Status

Current version: **0.7.x**. The project went through a full security and quality
audit (27 issues); all of them are now addressed. See
[`AUDIT_STATUS.md`](AUDIT_STATUS.md) for the issue-by-issue accounting and
[`CHANGELOG.md`](CHANGELOG.md) for the per-release history.

Retrieval is the headline feature set:

- **Hybrid search.** Each memory is stored as two vectors: a dense vector
  (multilingual-e5-base) for meaning and a sparse BM25 vector for exact terms. A
  query runs both and the results are merged with Reciprocal Rank Fusion, so you get
  semantic recall and exact-keyword precision at once.
- **Cross-encoder reranking.** The fused top candidates are re-scored by a
  cross-encoder (bge-reranker-base) that reads the query and passage together. On by
  default and strong on English.
- **Warm daemon.** A long-lived process keeps the models loaded and serves the MCP
  client over a Unix socket, so a lookup does not pay model load time on every call.

## Why

An assistant without memory repeats itself, asks for the same context again, and
cannot build on past work. The usual fix is manual notes, tags, and folders, which is
work you have to keep doing. MGI-Mind moves that to the assistant: it writes memories
as it goes and retrieves them by meaning when they are relevant. You own the data and
it stays local.

## How it works

```
  your AI assistant
        |  MCP (stdio)
        v
  mcp-server (Node)  --- Unix socket --->  mgimind daemon (Rust, models warm)
        |  fallback: spawn CLI                   |
        v                                        v
  mgimind (Rust CLI) ----------------------> Qdrant (local, loopback only)
                                                 |
                                  one "memories" collection:
                                  dense vector (e5) + sparse vector (BM25)
```

**Storage.** All memories live in a single Qdrant collection. A `library` field on
each point separates namespaces (for example work notes and game design in one place,
filtered when needed). Point IDs are a UUIDv5 of `library + content`, so adding the
same content twice is an idempotent upsert with no duplicates. A `created_at` datetime
index powers chronological history without scanning everything.

**Embeddings.** Text is embedded locally with ONNX Runtime. The default model is
multilingual-e5-base (768 dimensions), which is strong on English and also handles
mixed-language content. The embedder is model-aware: pooling (mean or CLS), the
token_type_ids input, and query/passage prefixes are all configurable, so swapping
models does not need code changes. Inputs are capped at 512 tokens.

**Search.** A query embeds once, then Qdrant runs a dense nearest-neighbor search and
a sparse BM25 search in one Query API call and fuses them with RRF. If reranking is
on, the top `rerank_top_k` candidates are re-scored by the cross-encoder and
re-ordered. A `library` filter, when given, applies to both arms.

**Safety.** Downloads are verified against pinned SHA-256 hashes where available.
Qdrant binds to loopback only and can take an API key. The secret vault is
terminal-only: the master password is read without echo, zeroized after use, and never
returned over the MCP channel. File writes are atomic (temp file plus rename), so a
crash cannot corrupt config, the vault, or sessions.

## Install (Linux)

Prerequisites: a Rust toolchain (`rustup`), and Node or Bun for the MCP server.

```bash
# 1. Clone and build
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release
# Binary: target/release/mgimind

# 2. Initialize and download the runtime + models
target/release/mgimind init
target/release/mgimind doctor --fix

# 3. Start the bundled Qdrant
target/release/mgimind serve

# 4. Verify
target/release/mgimind doctor
```

`doctor --fix` downloads, into `~/mgimind/`:

- ONNX Runtime (the shared library the embedder loads),
- the Qdrant server binary,
- the embedding model (multilingual-e5-base, quantized ONNX, about 270 MB),
- the reranker model (bge-reranker-base, quantized ONNX, about 280 MB).

The embedder finds ONNX Runtime automatically if it sits next to the binary;
otherwise set `ORT_DYLIB_PATH` to the `.so`/`.dylib`/`.dll`.

macOS and Windows build the same way (`cargo build --release`); `doctor --fix` picks
the right platform downloads.

### MCP server

The assistant talks to MGI-Mind through the Node MCP server in `mcp-server/`.

```bash
cd mcp-server
bun install        # or: npm install
```

Then add it to your assistant. For Claude Code:

```bash
claude mcp add mgi-mind -- node /absolute/path/to/mgi-mind/mcp-server/index.js
```

The MCP server prefers the warm daemon (run `mgimind daemon`) and falls back to
spawning the CLI if the daemon is not running, so it works either way.

## Commands

### Memory

| Command | What it does |
|---|---|
| `mgimind add <library> <content> [--source <tag>]` | Store a memory. |
| `mgimind search <query> [--library <l>] [--limit N] [--tier 1\|2\|3]` | Hybrid search + rerank. Tier controls how much text comes back. |
| `mgimind history [--limit N]` | Most recent memories, newest first. |
| `mgimind delete <library> <id>` | Delete one memory by id. |
| `mgimind context` | A compact session-start briefing (last session, key facts, libraries). |

### Libraries

| Command | What it does |
|---|---|
| `mgimind create <name>` | Register a library. |
| `mgimind list` | List libraries. |
| `mgimind drop <name>` | Delete a library and its memories. |
| `mgimind stats` | Counts per library, facts, sessions. |

### Knowledge graph

| Command | What it does |
|---|---|
| `mgimind fact add <subject> <predicate> <object>` | Store a fact triple. |
| `mgimind fact query <term>` | Find facts matching a term. |
| `mgimind fact invalidate <id>` | Soft-delete a fact (kept, marked invalid). |

### Sessions

| Command | What it does |
|---|---|
| `mgimind session start --agent <name>` | Begin a session for an agent. |
| `mgimind session end --agent <name> --summary <text>` | Close it with a summary. |
| `mgimind session last [--agent <name>]` | Show the last session. |

### Vault (terminal only)

| Command | What it does |
|---|---|
| `mgimind vault store <key> <value> [--category c] [--desc d]` | Store a secret (encrypted). |
| `mgimind vault get <key>` | Retrieve a secret (prompts for the master password). |
| `mgimind vault list` | List keys (values hidden). |
| `mgimind vault delete <key>` | Delete a secret. |

### Service and data

| Command | What it does |
|---|---|
| `mgimind serve` / `mgimind stop` | Start/stop the bundled Qdrant. |
| `mgimind daemon` | Run the warm daemon (keeps models loaded, serves the MCP client). |
| `mgimind migrate [--purge]` | Import legacy per-library collections into the single collection. Re-embeds from stored content, idempotent. `--purge` drops the old collections after. |
| `mgimind backup <file>` / `mgimind restore <file>` | gzip+tar of the data directory. |
| `mgimind export [--format json\|md] [--output dir]` | Export memories to files. |
| `mgimind import <obsidian\|markdown> <path> [--library l]` | Import a folder of markdown. |
| `mgimind doctor [--fix]` | Health check; `--fix` downloads what is missing. |

## Configuration

Config lives at `~/mgimind/config.json`. Fields that matter for retrieval:

| Field | Default | Meaning |
|---|---|---|
| `model_name` | `multilingual-e5-base` | Embedding model directory under `models/`. |
| `vector_size` | `768` | Embedding dimension. Must match the model. |
| `pooling` | `mean` | `mean` (e5, MiniLM) or `cls` (some XLM-R models). |
| `uses_token_type_ids` | `false` | True for BERT-family models, false for XLM-R/e5. |
| `query_prefix` / `passage_prefix` | `query: ` / `passage: ` | e5 needs these; leave empty for models that do not. |
| `rerank_enabled` | `true` | Cross-encoder reranking. Strong on English. |
| `rerank_model` | `bge-reranker-base` | Reranker directory under `models/`. |
| `rerank_top_k` | `20` | Candidates fetched and reranked before returning `limit`. |
| `qdrant_port` | `6334` | Qdrant gRPC port. |
| `qdrant_api_key` | none | If set, Qdrant starts with it and the client authenticates. |

### Notes on models and languages

- The default embedder is multilingual, so an English query can find content written
  in other languages and the other way around.
- The default reranker (bge-reranker-base) is tuned for English and improves English
  ranking. It lowers Russian ranking, so for Russian-heavy use either set
  `rerank_enabled=false` (hybrid dense+sparse alone ranks Russian well) or switch to a
  stronger multilingual reranker.
- Reranking adds cross-encoder inference per query. On a CPU-only box that is roughly
  one to two seconds for 20 candidates. Lower `rerank_top_k` or turn rerank off for
  lower latency.

### Switching the embedding model

Changing the model usually changes the vector dimension, which means existing memories
have to be re-embedded. Set `model_name`, `vector_size`, `pooling`,
`uses_token_type_ids`, and the prefixes for the new model, run `mgimind doctor --fix`
to fetch it, then re-embed with `mgimind migrate` (it re-embeds from the stored text).
Keep a backup first.

## Project layout

```
src/
  cli.rs         command dispatch and rendering
  storage.rs     Qdrant: collection, hybrid search, history, stats, migrate
  embedder.rs    ONNX embedding (model-aware pooling, prefixes, 512-token cap)
  reranker.rs    cross-encoder reranking
  knowledge.rs   knowledge-graph facts
  session.rs     per-agent session files
  vault.rs       encrypted secret vault (terminal only)
  daemon.rs      Unix-socket daemon (warm models)
  config.rs      config file
  integrity.rs   pinned SHA-256 hashes for downloads
  util.rs        atomic writes, verified downloads
mcp-server/
  index.js       MCP server (daemon client with CLI fallback)
```

## License

Apache-2.0. See [`LICENSE`](LICENSE).
