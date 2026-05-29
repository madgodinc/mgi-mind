# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

Long-term memory for AI assistants, self-hosted. Your assistant saves what matters as
you work and finds it again later by meaning, so it stops asking you the same things
and stops starting from zero every session. Everything runs on your machine: one Rust
binary, a local Qdrant vector database, and local ONNX models. No cloud, no API keys,
nothing leaves the box.

```
You:  what was the deploy server again?

Assistant (calls mind_search "deploy server"):
  -> "Deploy server is 10.0.0.5:8080, SSH as deploy@, key in vault"  (source: infra.md)

You:  right, thanks
```

It plugs into your assistant over MCP (Model Context Protocol), so Claude Code and
similar tools read and write memory on their own. It is also a normal CLI you can use
by hand.

---

## Contents

- [What it is](#what-it-is)
- [Why bother](#why-bother)
- [Quick start](#quick-start)
- [Using it](#using-it)
- [How it works](#how-it-works)
- [Command reference](#command-reference)
- [Configuration](#configuration)
- [Languages and the reranker](#languages-and-the-reranker)
- [Changing the embedding model](#changing-the-embedding-model)
- [Troubleshooting](#troubleshooting)
- [Security](#security)
- [Status and audit](#status-and-audit)
- [Project layout](#project-layout)
- [License](#license)

## What it is

MGI-Mind is the memory layer that sits between you and your assistant. The assistant
writes short notes ("memories") and facts as a conversation goes, and pulls the
relevant ones back when they matter. You do not tag, file, or organize anything.

It is built for retrieval quality, not just storage:

- **Hybrid search.** Every memory is stored as two vectors at once: a dense vector
  (multilingual-e5-base) that captures meaning, and a sparse term-frequency vector
  (TF-IDF, BM25-style) that captures exact words. A query runs both and the two
  ranked lists are merged with Reciprocal Rank Fusion. You get semantic recall ("the
  server box" finds "deploy host") and exact-term precision ("fossilize_replay" finds
  the one note that uses that word) in a single query.
- **Cross-encoder reranking.** The fused top candidates are re-scored by a
  cross-encoder (bge-reranker-base) that reads the query and the passage together,
  which is far more accurate than comparing vectors. It is on by default and is
  strong on English. (See [Languages and the reranker](#languages-and-the-reranker)
  for the trade-off on other languages.)
- **A warm daemon.** A long-lived process keeps the models loaded in memory and
  answers the assistant over a Unix socket, so a lookup costs milliseconds instead of
  reloading a model on every call.

Around retrieval there is a small knowledge graph for structured facts, per-agent
session logs for continuity, and an encrypted, terminal-only vault for secrets.

## Why bother

An assistant with no memory repeats itself, asks for the same context again, and can
never build on yesterday's work. The usual workaround is you keeping notes, tags, and
folders, which is a chore that never ends, and the assistant still cannot read them
well.

The alternatives have real gaps:

- **Plain notes (Obsidian, Notion).** Great for you, but the assistant cannot search
  them by meaning, and you do all the filing.
- **A bare vector database.** Gives you semantic search but no exact-term matching, no
  reranking, no dedup, no sessions, no facts, no secrets handling. You assemble all of
  that yourself.
- **A hosted "memory" API.** Sends your data to someone else's servers.

MGI-Mind is the assembled, local version: hybrid + reranked retrieval, dedup, facts,
sessions, and a vault, behind one binary you run yourself. You own the data and it
never leaves the machine.

## Quick start

You need a Rust toolchain (`rustup`) and Node or Bun (for the MCP server). Linux is
shown here; macOS and Windows build the same way.

```bash
# 1. Build
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release                  # binary: target/release/mgimind

# 2. Set up: create ~/mgimind/, then download Qdrant, ONNX Runtime, and the models
target/release/mgimind init
target/release/mgimind doctor --fix

# 3. Start the vector database and the warm daemon
target/release/mgimind serve           # bundled Qdrant (loopback only)
target/release/mgimind daemon &         # keeps models warm for fast lookups

# 4. Try it
target/release/mgimind create work
target/release/mgimind add work "Deploy server is 10.0.0.5:8080, SSH as deploy@"
target/release/mgimind search "how do I reach the deploy box"
```

`doctor --fix` downloads into `~/mgimind/`: ONNX Runtime (the library the embedder
loads), the Qdrant binary, the embedding model (multilingual-e5-base, quantized ONNX,
about 270 MB) and the reranker (bge-reranker-base, quantized ONNX, about 280 MB).

> **Run the daemon as a service.** Step 3 starts the daemon in the foreground with
> `&`. For day-to-day use, keep it running so the assistant never hits a cold start.
> A minimal systemd user unit:
>
> ```ini
> # ~/.config/systemd/user/mgimind-daemon.service
> [Unit]
> Description=MGI-Mind daemon
> After=network.target
> [Service]
> ExecStart=%h/mgi-mind/target/release/mgimind daemon
> Restart=on-failure
> [Install]
> WantedBy=default.target
> ```
>
> `systemctl --user enable --now mgimind-daemon`. Without the daemon everything still
> works; the MCP server just spawns the CLI per call and reloads the model each time.

### Connect your assistant (MCP)

```bash
cd mcp-server
bun install                            # or: npm install
```

Add it to Claude Code:

```bash
claude mcp add mgi-mind -- node /absolute/path/to/mgi-mind/mcp-server/index.js
```

The MCP server talks to the warm daemon if it is up and falls back to the CLI if not,
so it works either way. Point your assistant at [`AI_INSTRUCTIONS.md`](AI_INSTRUCTIONS.md)
once so it knows the protocol (log a session, search before answering, use the vault
for secrets).

## Using it

**By hand (CLI).**

```bash
mgimind add notes "Mom's birthday is March 14, she likes peonies" --source personal
mgimind search "when is my mother's birthday"
#  1. [notes] (score: 0.94) Mom's birthday is March 14, she likes peonies
#     source: personal

mgimind fact add "user" "prefers" "Rust"
mgimind fact query "user"
#   user -> prefers -> Rust

mgimind history --limit 3              # the three most recent memories
mgimind stats                          # counts per library, facts, sessions
```

**Through your assistant (MCP).** Once connected, you just talk. The assistant calls
the tools itself:

```
You:  remember that the staging DB password is in the vault under "staging-db"
Assistant: (mind_add) saved. The secret itself stays in your terminal vault, not here.

You:  what database are we using on staging?
Assistant: (mind_search "staging database") Postgres 16, host db-staging.internal:5432
```

Search returns results in tiers so the assistant spends tokens wisely: `--tier 1` is a
~100-character snippet, `--tier 2` (default) is ~500, `--tier 3` is the full text.

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
                                  one "memories" collection, two vectors per point:
                                  dense (e5, meaning) + sparse (TF-IDF, exact terms)
```

**Storage.** All memories live in one Qdrant collection. A `library` field on each
point separates namespaces (work, personal, a project), and a query can filter to one
library or search across all. A point's ID is a UUIDv5 of `library + content`, so
adding the same text twice just overwrites the same point: no duplicates, no race. A
`created_at` datetime index lets `history` return the newest N directly instead of
scanning everything.

**Embeddings.** Text is embedded locally through ONNX Runtime. The default is
multilingual-e5-base (768 dimensions), which is strong on English and handles mixed
languages. The embedder is model-aware: pooling (mean or CLS), the `token_type_ids`
input, and the query/passage prefixes are all config, so switching models needs no
code change. Inputs are capped at 512 tokens, and `add` splits long text into chunks
so nothing past the cap is silently dropped.

**Search.** The query is embedded once. Qdrant then runs a dense nearest-neighbor
search and a sparse search in a single Query API call and fuses them with RRF. If
reranking is on, the top `rerank_top_k` candidates are re-scored by the cross-encoder
and reordered. A `library` filter, when given, applies to both arms.

**Safety.** Downloads are checked against pinned SHA-256 hashes, fail-closed. Qdrant
binds to loopback only and can require an API key. The vault is terminal-only. Writes
are atomic (temp file, fsync, rename, fsync the directory), so a crash cannot corrupt
config, the vault, or sessions.

## Command reference

### Memory

| Command | What it does |
|---|---|
| `mgimind add <library> <content> [--source <tag>]` | Store a memory. Long text is chunked; prints how many chunks were stored. |
| `mgimind search <query> [--library <l>] [--limit N] [--tier 1\|2\|3]` | Hybrid search, then rerank. Tier sets how much text comes back. |
| `mgimind history [--limit N]` | Most recent memories, newest first. |
| `mgimind delete <library> <id>` | Delete one memory by id (id is shown in search results). |
| `mgimind context` | A compact session-start briefing: last session, recent facts, libraries. |

### Libraries

| Command | What it does |
|---|---|
| `mgimind create <name>` | Register a library. |
| `mgimind list` | List libraries. |
| `mgimind drop <name>` | Delete a library and all its memories. |
| `mgimind stats` | Counts per library, facts, sessions, vault state. |

### Knowledge graph

| Command | What it does |
|---|---|
| `mgimind fact add <subject> <predicate> <object>` | Store a fact triple. Same triple overwrites (dedup). |
| `mgimind fact query <term>` | Find facts matching a term in subject, predicate, or object. |
| `mgimind fact invalidate <id>` | Soft-delete a fact (kept on disk, marked invalid, hidden from queries). |

### Sessions

| Command | What it does |
|---|---|
| `mgimind session start --agent <name>` | Begin a session log for an agent. |
| `mgimind session end --agent <name> --summary <text>` | Close it with a summary. |
| `mgimind session last [--agent <name>]` | Show the last session (optionally for one agent). |

### Vault (terminal only)

| Command | What it does |
|---|---|
| `mgimind vault store <key> <value> [--category c] [--desc d]` | Store an encrypted secret. |
| `mgimind vault get <key>` | Retrieve a secret (prompts for the master password, then confirms). |
| `mgimind vault list` | List key names (values never shown). |
| `mgimind vault delete <key>` | Delete a secret. |

### Service and data

| Command | What it does |
|---|---|
| `mgimind serve` / `mgimind stop` | Start / stop the bundled Qdrant. |
| `mgimind daemon` | Run the warm daemon (loads models once, serves the MCP client). |
| `mgimind migrate [--purge]` | Re-embed legacy per-library collections into the single `memories` collection. Idempotent. `--purge` deletes the old collections afterward. |
| `mgimind backup <file>` / `mgimind restore <file>` | gzip+tar of the whole data directory. |
| `mgimind export [--format json\|md] [--output <dir>]` | Export memories to files. |
| `mgimind import <obsidian\|markdown> <path> [--library <l>]` | Import a folder of markdown (recursively, chunked). |
| `mgimind doctor [--fix]` | Health check; `--fix` downloads anything missing. |

## Configuration

Config is `~/mgimind/config.json`. The fields that affect retrieval:

| Field | Default | Meaning |
|---|---|---|
| `model_name` | `multilingual-e5-base` | Embedding model directory under `models/`. |
| `vector_size` | `768` | Embedding dimension. Must match the model. |
| `pooling` | `mean` | `mean` (e5, MiniLM) or `cls` (some XLM-R models). |
| `uses_token_type_ids` | `false` | `true` for BERT-family models, `false` for XLM-R / e5. |
| `query_prefix` / `passage_prefix` | `query: ` / `passage: ` | e5 needs these; empty for models that do not. |
| `rerank_enabled` | `true` | Cross-encoder reranking. See the note below. |
| `rerank_model` | `bge-reranker-base` | Reranker directory under `models/`. |
| `rerank_top_k` | `20` | How many candidates to fetch and rerank before returning `limit`. |
| `qdrant_port` | `6334` | Qdrant gRPC port. |
| `qdrant_api_key` | none | If set, Qdrant starts with it and the client authenticates. |

## Languages and the reranker

The default stack is tuned for English, because that is where the assistant itself
reasons best and where the models are strongest. It is the recommended setup for an
English-first project.

A few honest details:

- The embedder (multilingual-e5-base) is genuinely multilingual. An English query can
  find a Russian note and vice versa, and search alone works well across languages.
- The default reranker (bge-reranker-base) is English-tuned. It improves English
  ranking, but it **lowers ranking quality on Russian** (and other non-English
  languages). This is the one place the defaults favor English.
- **If your content is mostly Russian (or another non-English language):** set
  `rerank_enabled = false`. Hybrid dense+sparse search on its own ranks those
  languages well; the English-tuned reranker is what hurts them. Or swap in a stronger
  multilingual reranker.
- Reranking costs cross-encoder inference per query: roughly one to two seconds for 20
  candidates on a CPU-only box. Lower `rerank_top_k` or turn it off for snappier
  search.

## Changing the embedding model

Switching models usually changes the vector dimension, so existing memories must be
re-embedded:

1. Back up first: `mgimind backup ~/mgi-backup.tar.gz`.
2. Set `model_name`, `vector_size`, `pooling`, `uses_token_type_ids`, and the prefixes
   in `config.json` for the new model.
3. `mgimind doctor --fix` to download it. (Only the bundled defaults are pinned; a
   custom model downloads with an "integrity not verified" warning, so pin its
   SHA-256 in `integrity.rs` if you care.)
4. `mgimind migrate` to re-embed everything from the stored text under the new model.

## Troubleshooting

- **"Model not found ... run doctor --fix".** The model is not in `~/mgimind/models/`.
  Run `mgimind doctor --fix`.
- **"invalid expand shape" / inference errors.** Usually an input far over 512 tokens.
  `add` chunks automatically; if you call the library directly, chunk first.
- **Searches are slow.** That is the reranker on CPU. Lower `rerank_top_k`, or set
  `rerank_enabled = false`. Make sure the daemon is running so models stay warm.
- **Cold start on every call.** The daemon is not running. Start `mgimind daemon` (see
  the service note in Quick start).
- **Dimension mismatch warning on `serve`.** A collection's vectors do not match
  `vector_size`, usually after a model change. Re-embed with `mgimind migrate`.
- **Russian results feel off.** Set `rerank_enabled = false` (see the language note).

## Security

- Downloads are verified against pinned SHA-256 (fail-closed) for ONNX Runtime
  (linux-x64), Qdrant, and the default models (e5 and the reranker). Other platforms
  and custom models warn instead of trusting blindly.
- Qdrant binds to `127.0.0.1` only and supports an API key.
- The vault is AES-256-GCM with an Argon2id-derived key (parameters pinned so a
  library upgrade cannot lock you out). It is terminal-only: the master password and
  decrypted secrets never travel over the MCP channel, and the daemon socket is
  owner-only (0600).
- File writes are atomic and directory-fsynced, so a crash leaves the old file or the
  new one, never a corrupt one.

## Status and audit

Current version: **0.7.x**. The project went through a full code audit (27 issues):
**21 are fully fixed and 6 are partial** (the mechanism shipped, hardening continues).
[`AUDIT_STATUS.md`](AUDIT_STATUS.md) accounts for every issue one by one, including the
honest gaps (for example, fact supersession is not implemented yet).
[`CHANGELOG.md`](CHANGELOG.md) has the per-release history.

## Project layout

```
src/
  cli.rs         command dispatch and output rendering
  storage.rs     Qdrant: single collection, hybrid search, history, stats, migrate, chunking
  embedder.rs    ONNX embedding (model-aware pooling, prefixes, 512-token cap)
  reranker.rs    cross-encoder reranking
  knowledge.rs   knowledge-graph facts
  session.rs     per-agent session files
  vault.rs       encrypted secret vault (terminal only)
  daemon.rs      Unix-socket daemon (warm models)
  config.rs      configuration
  integrity.rs   pinned SHA-256 hashes for downloads
  util.rs        atomic writes, verified downloads
mcp-server/
  index.js       MCP server (daemon client with CLI fallback)
tests/
  cli_integration.rs   black-box tests against a real Qdrant
```

## License

Apache-2.0. See [`LICENSE`](LICENSE).
