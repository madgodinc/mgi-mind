# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

Your AI assistant forgets everything after each session. MGI-Mind fixes that.

It's a self-hosted memory system that sits between you and your AI. The AI stores what matters, finds it later by meaning, and picks up where it left off. No manual note-taking. No tagging. No folder hierarchies. You talk to your AI - it remembers.

```
"What was the server address?"

-> mgimind search "server address"
-> Deploy server is 10.0.0.5:8080 (score: 0.72)
```

---

## Table of Contents

- [Why](#why)
- [Quick Start](#quick-start)
- [Architecture](#architecture)
- [Commands](#commands)
  - [Setup](#setup)
  - [Memory](#memory)
  - [Knowledge Graph](#knowledge-graph)
  - [Sessions](#sessions)
  - [Vault](#vault)
  - [Web Reader](#web-reader)
  - [Import / Export](#import--export)
  - [Utilities](#utilities)
- [MCP Integration](#mcp-integration)
- [AI Instructions & Self-Configuration](#ai-instructions--self-configuration)
- [Tiered Retrieval](#tiered-retrieval)
- [Stack](#stack)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [Contributing](#contributing)
- [License](#license)

---

## Why

Every AI assistant today has amnesia. Claude, Cursor, Copilot - they start fresh every session. The usual workarounds:

| Approach | Problem |
|----------|---------|
| Paste context manually | Tedious, easy to forget, wastes tokens |
| Obsidian/Notion + copy-paste | Manual labor, AI can't search it |
| RAG with vector DB | Complex setup, no session continuity |
| System prompts | Limited size, no semantic search |

MGI-Mind combines all of these into one tool that the AI manages itself:

- **Semantic memory** - vector search by meaning, not keywords
- **Knowledge graph** - structured facts like `user -> prefers -> Rust`
- **Session logs** - AI reads what happened last time, writes what happened this time
- **Encrypted vault** - passwords and API keys, separate from searchable memory
- **Self-configuration** - AI suggests adding permanent rules to its own config file
- **Web reading** - AI can read any webpage and save it to memory

All self-hosted. All local. No cloud. No API keys needed for basic operation.

---

## Quick Start

No Docker needed. No Python. No Node (except Bun for MCP). Same steps on every OS.

### Windows

```powershell
# 1. Install Rust (if you don't have it)
#    Download from https://rustup.rs/ and run rustup-init.exe
#    Or in terminal:
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# 2. Install Bun (for MCP server)
powershell -c "irm bun.sh/install.ps1 | iex"

# 3. Install CRW (web reader)
cargo install crw-mcp crw-cli

# 4. Clone and build
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release
# Binary: target\release\mgimind.exe

# 5. Set up
mgimind init
mgimind doctor --fix    # Downloads: Qdrant (~29MB), ONNX Runtime (~71MB), model (~44MB)
mgimind serve           # Starts Qdrant on port 6333/6334

# 6. Install MCP server
cd mcp-server && bun install && cd ..

# 7. Verify
mgimind doctor
# All checks passed.
```

### macOS

```bash
# 1. Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# 2. Install Bun
curl -fsSL https://bun.sh/install | bash
source ~/.bashrc

# 3. Install CRW
cargo install crw-mcp crw-cli

# 4. Clone and build
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release
# Binary: target/release/mgimind

# 5. Set up (auto-detects Intel vs Apple Silicon)
./target/release/mgimind init
./target/release/mgimind doctor --fix
./target/release/mgimind serve

# 6. Install MCP server
cd mcp-server && bun install && cd ..

# 7. Verify
./target/release/mgimind doctor
```

### Linux (Ubuntu/Debian)

```bash
# 1. Install build tools (if missing)
sudo apt update && sudo apt install -y curl build-essential pkg-config libssl-dev

# 2. Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# 3. Install Bun
curl -fsSL https://bun.sh/install | bash
source ~/.bashrc

# 4. Install CRW
cargo install crw-mcp crw-cli

# 5. Clone and build
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release
# Binary: target/release/mgimind

# 6. Set up
./target/release/mgimind init
./target/release/mgimind doctor --fix
./target/release/mgimind serve

# 7. Install MCP server
cd mcp-server && bun install && cd ..

# 8. Verify
./target/release/mgimind doctor
```

### Linux Server (headless VPS)

Same as Linux above. For production, run Qdrant as a systemd service:

```bash
# Create systemd service for Qdrant
sudo tee /etc/systemd/system/mgimind-qdrant.service > /dev/null <<'EOF'
[Unit]
Description=MGI-Mind Qdrant
After=network.target

[Service]
Type=simple
User=YOUR_USER
ExecStart=/home/YOUR_USER/.cargo/bin/qdrant
Environment=QDRANT__STORAGE__STORAGE_PATH=/home/YOUR_USER/mgimind/qdrant/storage
Restart=always

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable mgimind-qdrant
sudo systemctl start mgimind-qdrant
```

### What `doctor --fix` Downloads

Everything is platform-specific and automatic:

| Component | Windows | macOS Intel | macOS ARM | Linux x64 | Linux ARM |
|-----------|---------|-------------|-----------|-----------|-----------|
| Qdrant | `.exe` (zip) | binary (tar.gz) | binary (tar.gz) | binary (tar.gz) | binary (tar.gz) |
| ONNX Runtime | `.dll` (zip) | `.dylib` (tgz) | `.dylib` (tgz) | `.so` (tgz) | `.so` (tgz) |
| Model | `model.onnx` + `tokenizer.json` - same on all platforms |

Total download: ~144MB. All stored locally in `~/mgimind/` and next to the binary.

### First Run (all platforms)

After installation, the flow is identical everywhere:

```bash
mgimind doctor
# [OK]   Config exists
# [OK]   Sessions dir
# [OK]   Models dir
# [OK]   Qdrant data directory
# [OK]   Qdrant binary
# [OK]   Qdrant server (running)
# [OK]   ONNX Runtime
# [OK]   Embedding model
# All checks passed.
```

### Try It

```bash
# Create a library
mgimind create work

# Add memories
mgimind add work "Deploy server is 10.0.0.5, port 8080"
mgimind add work "Frontend uses React with TypeScript"
mgimind add work "CI/CD pipeline runs on GitHub Actions"

# Search by meaning (not keywords)
mgimind search "server address"
# 1. [work] (score: 0.724) id: 6ef4f51f-...
#    Deploy server is 10.0.0.5, port 8080

mgimind search "what tech stack"
# 1. [work] (score: 0.412) id: 3b7a2aa8-...
#    Frontend uses React with TypeScript
# 2. [work] (score: 0.318) id: a1c2d3e4-...
#    CI/CD pipeline runs on GitHub Actions
```

---

## Architecture

```
┌─────────────────────────────────────────────────┐
│  User talks to AI                               │
│  (Claude Code, Cursor, Cline, or any MCP client)│
└──────────────┬──────────────────────────────────┘
               │ MCP protocol (stdio)
┌──────────────▼──────────────────────────────────┐
│  MCP Server (Bun)                               │
│  20 tools: search, add, facts, vault, web...    │
│  Thin layer - just calls mgimind CLI            │
└──────────────┬──────────────────────────────────┘
               │ subprocess
┌──────────────▼──────────────────────────────────┐
│  mgimind (Rust binary)                          │
│  ┌──────────────────────────────────────────┐   │
│  │ Embedder    │ ONNX Runtime + MiniLM-L6   │   │
│  │ Storage     │ Qdrant client              │   │
│  │ KG          │ Subject-Predicate-Object   │   │
│  │ Sessions    │ Markdown log files         │   │
│  │ Vault       │ AES-256-GCM + Argon2      │   │
│  │ Web         │ CRW (Rust web reader)      │   │
│  └──────────────────────────────────────────┘   │
└──────────────┬──────────────────────────────────┘
               │
┌──────────────▼──────────────────────────────────┐
│  Qdrant (vector database, also Rust)            │
│  Collections: mem_work, mem_notes, _kg_facts    │
│  Storage: ~/mgimind/qdrant/                     │
└─────────────────────────────────────────────────┘
```

### How Semantic Search Works

When you store text, MGI-Mind converts it to a 384-dimensional vector using a neural network:

```
"Deploy server is 10.0.0.5:8080"
    -> [0.23, -0.15, 0.87, 0.04, ..., -0.31]  (384 floats)
```

When you search, your query becomes a vector too:

```
"server address"
    -> [0.21, -0.18, 0.85, 0.06, ..., -0.28]  (384 floats)
```

Qdrant compares vectors using cosine similarity. Close vectors = similar meaning. The words don't need to match - the meaning does.

### Data Layout

```
~/mgimind/
├── config.json              # Settings (qdrant port, model name)
├── vault.enc                # AES-256-GCM encrypted secrets
├── vault.salt               # Argon2 salt (32 bytes)
├── vault.count              # Number of secrets (unencrypted, for stats)
├── sessions/
│   ├── 2026-05-28_14-30_claude-code.md
│   ├── 2026-05-29_09-00_cursor.md
│   └── .current             # Pointer to active session
├── models/
│   └── all-MiniLM-L6-v2/
│       ├── model.onnx       # Neural network (44MB)
│       └── tokenizer.json   # Tokenizer config
└── qdrant/
    └── storage/             # Vector database files
```

---

## Commands

### Setup

#### `mgimind init`

Creates the `~/mgimind/` directory structure and config file. Safe to run multiple times.

```bash
$ mgimind init
MGI-Mind initialized at /home/user/mgimind
  Data:     /home/user/mgimind
  Sessions: /home/user/mgimind/sessions
  Models:   /home/user/mgimind/models
```

#### `mgimind doctor [--fix]`

Checks system health. With `--fix`, automatically downloads missing components.

```bash
$ mgimind doctor --fix
[OK]   Config exists
[OK]   Sessions dir
[OK]   Models dir
[OK]   Qdrant data directory
[FAIL] Qdrant binary not found
       Downloading Qdrant...
       Qdrant installed to /home/user/.cargo/bin/qdrant
[OK]   Qdrant server (running)
[FAIL] ONNX Runtime not found
       Installing ONNX Runtime...
       ONNX Runtime installed
[FAIL] Embedding model not downloaded
       Downloading model...
       Model downloaded
Fixed 3 issue(s).
```

What it downloads (cross-platform):

| Component | Windows | macOS Intel | macOS ARM | Linux |
|-----------|---------|-------------|-----------|-------|
| Qdrant | qdrant.exe | qdrant | qdrant | qdrant |
| ONNX Runtime | onnxruntime.dll | libonnxruntime.dylib | libonnxruntime.dylib | libonnxruntime.so |
| Model | model.onnx + tokenizer.json (same for all platforms) |

#### `mgimind serve` / `mgimind stop`

Start and stop the bundled Qdrant server.

```bash
$ mgimind serve
Starting Qdrant...
Qdrant started on port 6333/6334 (PID: 12345)

$ mgimind stop
Stopping Qdrant (PID: 12345)...
Qdrant stopped.
```

---

### Memory

#### `mgimind create <name>`

Create a new library. Libraries are like databases - group related memories.

```bash
$ mgimind create work
Library 'work' created.

$ mgimind create personal
Library 'personal' created.

$ mgimind list
Libraries:
  - personal
  - work
```

#### `mgimind add <library> "<content>" [--source "<tag>"]`

Store a memory. Generates embedding automatically. Deduplicates - won't store the same text twice.

```bash
$ mgimind add work "Deploy server is 10.0.0.5, SSH port 22"
Added to 'work' [id: 6ef4f51f-efc0-48ef-bfa4-213dedc9e3d9]

$ mgimind add work "Deploy server is 10.0.0.5, SSH port 22"
Error: Duplicate content already exists [id: 6ef4f51f-...]

$ mgimind add work "React frontend deployed on Vercel" --source "standup-notes"
Added to 'work' [id: 3b7a2aa8-d127-4d01-b01f-530259b7e272]
```

#### `mgimind search "<query>" [--library <name>] [--limit N] [--tier 1|2|3]`

Semantic search across all libraries (or filter by one).

```bash
$ mgimind search "server address"
1. [work] (score: 0.724) id: 6ef4f51f-...
   Deploy server is 10.0.0.5, SSH port 22

2. [work] (score: 0.318) id: 3b7a2aa8-...
   React frontend deployed on Vercel
   source: standup-notes

# Search only in one library
$ mgimind search "server" --library work --limit 1
1. [work] (score: 0.724) id: 6ef4f51f-...
   Deploy server is 10.0.0.5, SSH port 22

# Tier 1 = short (max 100 chars), good for quick lookups
$ mgimind search "server" --tier 1
1. [work] (score: 0.724) id: 6ef4f51f-...
   Deploy server is 10.0.0.5, SSH port 22
```

#### `mgimind delete <library> <id>`

Remove a specific memory by its UUID.

```bash
$ mgimind delete work 6ef4f51f-efc0-48ef-bfa4-213dedc9e3d9
Deleted from 'work' [id: 6ef4f51f-efc0-48ef-bfa4-213dedc9e3d9]
```

#### `mgimind drop <name>`

Delete an entire library and all its memories.

```bash
$ mgimind drop work
Library 'work' dropped.
```

---

### Knowledge Graph

Structured facts stored as subject-predicate-object triples. Searchable semantically.

#### `mgimind fact add "<subject>" "<predicate>" "<object>"`

```bash
$ mgimind fact add "user" "prefers_language" "Rust"
Fact added: user -> prefers_language -> Rust [id: c633370a-...]

$ mgimind fact add "project" "deployed_on" "10.0.0.5:8080"
Fact added: project -> deployed_on -> 10.0.0.5:8080 [id: 215a9a1e-...]

$ mgimind fact add "frontend" "uses" "React+TypeScript"
Fact added: frontend -> uses -> React+TypeScript [id: 44b5c4a8-...]
```

#### `mgimind fact query "<subject>"`

```bash
$ mgimind fact query "user"
  user -> prefers_language -> Rust
    added: 2026-05-28T12:12:12+00:00
```

#### `mgimind fact invalidate "<id>"`

Remove a fact that's no longer true.

```bash
$ mgimind fact invalidate "c633370a-e675-4c52-8d03-509b0e764e03"
Fact 'c633370a-...' invalidated.
```

---

### Sessions

Session logs provide continuity between AI interactions.

```bash
# Start logging
$ mgimind session start --agent claude-code
Session started (agent: claude-code)

# Read the previous session
$ mgimind session last
[session]
agent = claude-code
started = 2026-05-28T14:30:00+00:00
status = completed

---

---

[end]
ended = 2026-05-28T16:00:00+00:00
summary = Discussed MGI-Mind architecture. Decided on Qdrant + Rust. Built CLI with 18 commands.

# End with a summary
$ mgimind session end --summary "Added vault encryption and web reader. All tests pass."
Session ended.
```

Session files are stored as Markdown in `~/mgimind/sessions/`.

---

### Vault

Encrypted storage for secrets. Uses AES-256-GCM with Argon2 key derivation. Completely separate from regular memory - secrets never appear in search results.

```bash
# First time: set master password
$ mgimind vault store ssh-prod "root:S3cret!" --category ssh --desc "Production server"
Set master password for vault: ********
Confirm master password: ********
Secret stored: ssh-prod [ssh]

# Store more secrets (asks master password each time)
$ mgimind vault store github-token "ghp_abc123" --category api-key --desc "GitHub PAT"
Master password: ********
Secret stored: github-token [api-key]

# List keys (values are NEVER shown in list)
$ mgimind vault list
Master password: ********
Vault secrets:
  [api-key] github-token - GitHub PAT
  [ssh] ssh-prod - Production server

# Retrieve a secret (asks for confirmation)
$ mgimind vault get ssh-prod
Master password: ********
=== VAULT ACCESS REQUEST ===
Key:         ssh-prod
Category:    ssh
Description: Production server
============================
Allow access? [y/N]: y
root:S3cret!

# Wrong password = denied
$ mgimind vault list
Master password: wrong
Error: Decryption failed - wrong master password?

# Delete a secret
$ mgimind vault delete ssh-prod
Master password: ********
Secret 'ssh-prod' deleted.
```

#### How Vault Encryption Works

```
Master password "MyP@ss123"
        │
        ▼
   Argon2 KDF + 32-byte random salt
        │
        ▼
   256-bit AES key
        │
        ▼
   AES-256-GCM encrypt(vault JSON)
        │
        ▼
   vault.enc = [12-byte nonce][ciphertext][auth tag]
```

The salt is stored in `vault.salt`. Without the master password, the vault file is unreadable binary.

---

### Web Reader

Read any webpage as clean Markdown. Powered by [CRW](https://github.com/us/crw) (Rust).

```bash
# Install CRW (one-time)
cargo install crw-mcp crw-cli

# Read a page
$ mgimind web "https://docs.rs/tokio/latest/tokio/"
# Tokio
Tokio is an asynchronous runtime for Rust...

# Read and save to a library
$ mgimind web "https://docs.rs/tokio/latest/tokio/" --save docs
Saved 12 chunks from https://docs.rs/tokio/latest/tokio/ to 'docs'

# Now you can search it
$ mgimind search "async runtime" --library docs
1. [docs] (score: 0.812) id: a1b2c3d4-...
   Tokio is an asynchronous runtime for Rust...
   source: https://docs.rs/tokio/latest/tokio/
```

CRW bypasses most bot protection (Cloudflare, etc.) automatically using Chrome-level TLS fingerprinting.

---

### Import / Export

#### Import from Obsidian or Markdown

```bash
# Import an Obsidian vault
$ mgimind import obsidian /path/to/my-vault --library notes
Created library 'notes'
Found 342 markdown files in /path/to/my-vault
Import complete: 1,247 chunks imported, 23 skipped

# Import any folder of .md files
$ mgimind import markdown /path/to/docs --library documentation
```

The importer:
- Scans `.md` files recursively
- Skips hidden directories (`.obsidian`, `.trash`)
- Chunks long files into ~500 character segments
- Tags each chunk with the source filename
- Deduplicates automatically

#### Export

```bash
# Export as JSON
$ mgimind export --format json --output ./backup
Exported 1,247 entries to ./backup/
# Creates: backup/work.json, backup/notes.json, backup/_kg_facts.json

# Export as Markdown
$ mgimind export --format md --output ./backup-md
Exported 1,247 entries to ./backup-md/
# Creates: backup-md/work.md, backup-md/notes.md
```

JSON export format:
```json
[
  {
    "id": "6ef4f51f-efc0-48ef-bfa4-213dedc9e3d9",
    "content": "Deploy server is 10.0.0.5, SSH port 22",
    "source": null,
    "created_at": "2026-05-28T12:00:00+00:00"
  }
]
```

---

### Utilities

#### `mgimind context`

Generates a compact briefing for AI session start. Everything the AI needs in one call.

```bash
$ mgimind context
=== MGI-Mind Context ===

[Last Session]
agent = claude-code
started = 2026-05-28T14:30:00+00:00
status = completed
summary = Built vault encryption, added web reader.

[Knowledge Graph - 3 facts]
  user -> prefers_language -> Rust
  project -> deployed_on -> 10.0.0.5:8080
  frontend -> uses -> React+TypeScript

[Libraries]
  work: 45 memories
  personal: 12 memories
  docs: 1247 memories

[Vault: 2 secrets stored]

=== End Context ===
```

#### `mgimind history [--limit N]`

```bash
$ mgimind history --limit 3
Recent memories:
1. [work] React frontend deployed on Vercel
   source: standup-notes
2. [work] Deploy server is 10.0.0.5, SSH port 22
3. [docs] Tokio is an asynchronous runtime for Rust...
   source: https://docs.rs/tokio/latest/tokio/
```

#### `mgimind stats`

```bash
$ mgimind stats
MGI-Mind Statistics
-------------------
Libraries:  3
  docs: 1247 memories
  personal: 12 memories
  work: 45 memories
Total memories: 1304
KG facts:       3
Sessions:       7
Vault secrets:  2
```

#### `mgimind backup <file>` / `mgimind restore <file>`

```bash
$ mgimind backup ./mgimind-backup.tar.gz
Backing up to ./mgimind-backup.tar.gz...
Backup complete.

$ mgimind restore ./mgimind-backup.tar.gz
Restoring from ./mgimind-backup.tar.gz...
Restore complete.
```

---

## MCP Integration

MGI-Mind exposes 20 tools via [Model Context Protocol](https://modelcontextprotocol.io/). Works with any MCP-compatible AI client.

### Setup

Install the MCP server dependencies:

```bash
cd mgi-mind/mcp-server
bun install
```

Add to your AI client's MCP config:

**Claude Code** (`~/.claude.json` or project's `.mcp.json`):
```json
{
  "mcpServers": {
    "mgi-mind": {
      "command": "bun",
      "args": ["run", "/absolute/path/to/mgi-mind/mcp-server/index.js"]
    },
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

**Cursor** (`.cursor/mcp.json`):
```json
{
  "mcpServers": {
    "mgi-mind": {
      "command": "bun",
      "args": ["run", "/absolute/path/to/mgi-mind/mcp-server/index.js"]
    }
  }
}
```

### Available MCP Tools

| Tool | Description |
|------|-------------|
| `mind_search` | Semantic search with tier and library filters |
| `mind_add` | Store a memory |
| `mind_delete` | Remove a memory by ID |
| `mind_create` | Create a library |
| `mind_list` | List libraries |
| `mind_fact_add` | Add a KG fact |
| `mind_fact_query` | Query KG facts |
| `mind_session_start` | Start session log |
| `mind_session_last` | Read last session |
| `mind_session_end` | End session with summary |
| `mind_vault_store` | Store an encrypted secret |
| `mind_vault_get` | Retrieve a secret |
| `mind_vault_list` | List secret keys |
| `mind_context` | Get compact briefing |
| `mind_history` | Recent additions |
| `mind_stats` | Memory statistics |
| `mind_web` | Read webpage as markdown |
| `mind_import` | Import from Obsidian/markdown |
| `mind_export` | Export to JSON/markdown |
| `mind_doctor` | Health check |

---

## AI Instructions & Self-Configuration

The file `AI_INSTRUCTIONS.md` is the brain of MGI-Mind. Any AI assistant reads it and knows:

- How to install and set up MGI-Mind
- The session protocol (read last session -> work -> write summary)
- When to use memory vs. facts vs. vault
- The 4-tier memory hierarchy

### 4-Tier Memory Hierarchy

```
Tier 0: AI config (CLAUDE.md / .cursorrules)
  -> Permanent rules, identity, workflow preferences
  -> Zero tokens per query - always loaded
  -> Example: "Always use Rust for new projects"

Tier 1: Knowledge Graph facts
  -> Structured: subject -> predicate -> object
  -> 10-50 tokens per lookup
  -> Example: user -> prefers -> dark_theme

Tier 2: Memories (default search tier)
  -> Full text, details, context
  -> 100-500 tokens per result
  -> Example: "The deploy process requires SSH to 10.0.0.5..."

Tier 3: Vault (secrets)
  -> AES-256-GCM encrypted
  -> Only retrieved on explicit request + master password
  -> Example: SSH password, API keys
```

### Self-Configuration Protocol

When the user establishes a permanent preference, the AI should do both:

1. Save to MGI-Mind: `mgimind fact add "user" "prefers" "Rust"`
2. Suggest adding to AI config file:

```
I saved this to memory. I also recommend adding it to CLAUDE.md
so every session knows this from the start:

  "Priority language: Rust for all new projects"

Want me to add it?
```

This way, permanent rules go into the system prompt (zero lookup cost) while details stay in searchable memory.

---

## Tiered Retrieval

The `--tier` flag on search controls how much text is returned:

| Tier | Max chars | Tokens | Use case |
|------|-----------|--------|----------|
| 1 | 100 | ~20-50 | Quick fact lookups, "what was the server IP?" |
| 2 | 500 | ~100-200 | Default. Good balance of detail and cost |
| 3 | Unlimited | ~500+ | Full context when you need every detail |

In practice, this means:

```bash
# Tier 1: quick answer
$ mgimind search "server IP" --tier 1
1. [work] (score: 0.72) id: 6ef4f51f-...
   Deploy server is 10.0.0.5, SSH port 22

# Tier 3: full context
$ mgimind search "deploy process" --tier 3
1. [work] (score: 0.68) id: a1b2c3d4-...
   The full deployment process: First SSH into 10.0.0.5 with the
   credentials from vault. Then cd /opt/app and run git pull.
   After that, docker-compose up -d --build. Check logs with
   docker-compose logs -f. The health endpoint is /api/health.
   If it returns 200, notify the team in #deploys channel...
```

An AI using Tier 2 by default saves 10-20x tokens compared to dumping all context.

---

## Stack

| Component | Technology | Why this |
|-----------|-----------|----------|
| Core engine | **Rust** | Single binary, zero runtime deps, memory-safe, compiler catches bugs |
| Vector DB | **Qdrant** | Written in Rust, fast, payload filtering, snapshots |
| Embeddings | **ONNX Runtime** + all-MiniLM-L6-v2 | Runs on CPU, no GPU needed, 384-dim vectors |
| MCP server | **Bun** | Fast JS runtime, thin layer calling Rust binary |
| Web reader | **CRW** | Rust, reads any page as Markdown, bypasses bot protection |
| Vault encryption | **AES-256-GCM** + **Argon2** | Industry standard, password-derived keys |
| Deduplication | **BLAKE3** | Fast hash, prevents duplicate entries |
| License | **Apache 2.0** | Use it commercially, fork it, modify it |

---

## Configuration

Config file: `~/mgimind/config.json`

```json
{
  "version": "0.1.0",
  "data_dir": "/home/user/mgimind",
  "model_name": "all-MiniLM-L6-v2",
  "qdrant_port": 6334
}
```

| Field | Default | Description |
|-------|---------|-------------|
| `data_dir` | `~/mgimind` | Where all data lives |
| `model_name` | `all-MiniLM-L6-v2` | ONNX embedding model |
| `qdrant_port` | `6334` | Qdrant gRPC port |

### Environment Variables

| Variable | Description |
|----------|-------------|
| `ORT_DYLIB_PATH` | Path to ONNX Runtime library (auto-detected if next to binary) |
| `RUST_LOG` | Log level: `debug`, `info`, `warn`, `error` |

---

## Troubleshooting

### "Failed to connect to Qdrant"

Qdrant isn't running. Start it:
```bash
mgimind serve
```

### "Model not found"

Run `mgimind doctor --fix` to download the embedding model.

### "Decryption failed - wrong master password?"

You entered the wrong vault master password. There's no password recovery - if you forget it, the vault contents are lost.

### Slow first search

The ONNX model takes 2-5 seconds to load on first use. Subsequent searches are fast (~50ms).

### CRW not found

Install the web reader:
```bash
cargo install crw-mcp crw-cli
```

### Windows: "Qdrant not starting"

If `mgimind serve` fails, try running Qdrant manually:
```bash
qdrant.exe
```
Check if port 6334 is already in use by another process.

---

## Server Deployment (Linux)

For running on a VPS or dedicated server:

```bash
# 1. Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# 2. Install Bun
curl -fsSL https://bun.sh/install | bash
source ~/.bashrc

# 3. Clone and build
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind && cargo build --release

# 4. Install CRW
cargo install crw-mcp crw-cli

# 5. Initialize
./target/release/mgimind init
./target/release/mgimind doctor --fix
./target/release/mgimind serve

# 6. Set up MCP server
cd mcp-server && bun install
```

---

## Contributing

MGI-Mind is Apache 2.0 licensed. Contributions welcome.

### Project Structure

```
mgi-mind/
├── src/
│   ├── main.rs        # Entry point, ORT auto-detection
│   ├── cli.rs         # 22 CLI commands, argument parsing
│   ├── config.rs      # ~/mgimind/ paths and config
│   ├── storage.rs     # Qdrant operations, search, dedup
│   ├── embedder.rs    # ONNX model loading, inference, pooling
│   ├── knowledge.rs   # Knowledge graph (facts)
│   ├── session.rs     # Session logging
│   ├── vault.rs       # AES-256-GCM encrypted secrets
│   └── error.rs       # Error types
├── mcp-server/
│   ├── index.js       # MCP server (20 tools)
│   └── package.json   # Bun dependencies
├── AI_INSTRUCTIONS.md # Instructions for AI assistants
├── README.md          # This file
├── README.ru.md       # Russian
├── README.zh.md       # Chinese
├── Cargo.toml         # Rust dependencies
└── LICENSE            # Apache 2.0
```

### Building

```bash
cargo build --release --target x86_64-pc-windows-msvc   # Windows
cargo build --release                                     # macOS/Linux
```

### Running Tests

```bash
mgimind doctor    # Verify all components
```

---

## License

Apache 2.0 - [Mad God Inc](https://github.com/madgodinc), 2026

Use it. Fork it. Build on it. Just keep the license.
