# MGI-Mind - AI Instructions

You are connecting to MGI-Mind, an AI-native second brain.
Read this file completely before proceeding.

## What is MGI-Mind

A self-hosted, token-optimized memory system for AI assistants.
All data stays on the user's machine. You are the interface.

## Installation

### Platform Detection
Detect the user's platform first:
- **Windows**: `uname -s` returns MINGW*/MSYS*/CYGWIN* or check for `C:\`
- **macOS**: `uname -s` returns Darwin
- **Linux**: `uname -s` returns Linux

### Prerequisites
- Rust toolchain (install via https://rustup.rs if missing)
- Qdrant vector database (see Qdrant Setup below)
- ONNX Runtime library (downloaded automatically by `mgimind doctor --fix`)

### Install from source
```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release
```

Binary location by platform:
- **Windows**: `target/release/mgimind.exe`
- **macOS/Linux**: `target/release/mgimind`

### Qdrant Setup

#### Option A: Docker (all platforms)
```bash
docker run -d -p 6333:6333 -p 6334:6334 \
  -v ~/mgimind/qdrant:/qdrant/storage \
  --name mgimind-qdrant \
  qdrant/qdrant
```

#### Option B: Binary (no Docker)

Download from https://github.com/qdrant/qdrant/releases/latest

| Platform | File |
|----------|------|
| Windows | `qdrant-x86_64-pc-windows-msvc.zip` |
| macOS (Intel) | `qdrant-x86_64-apple-darwin.tar.gz` |
| macOS (Apple Silicon) | `qdrant-aarch64-apple-darwin.tar.gz` |
| Linux | `qdrant-x86_64-unknown-linux-gnu.tar.gz` |

Extract and run:
```bash
./qdrant  # Starts on port 6333/6334 by default
```

### ONNX Runtime Setup

`mgimind doctor --fix` downloads this automatically. If manual install needed:

Download from https://github.com/microsoft/onnxruntime/releases/tag/v1.24.2

| Platform | File |
|----------|------|
| Windows | `onnxruntime-win-x64-1.24.2.zip` -> extract `onnxruntime.dll` next to `mgimind.exe` |
| macOS (Intel) | `onnxruntime-osx-x86_64-1.24.2.tgz` -> extract `libonnxruntime.dylib` |
| macOS (Apple Silicon) | `onnxruntime-osx-arm64-1.24.2.tgz` -> extract `libonnxruntime.dylib` |
| Linux | `onnxruntime-linux-x64-1.24.2.tgz` -> extract `libonnxruntime.so` |

Set environment variable:
```bash
# Windows
set ORT_DYLIB_PATH=C:\path\to\onnxruntime.dll

# macOS/Linux
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so  # or .dylib
```

### First-time setup
```bash
mgimind init          # Creates ~/mgimind/ with config, sessions, models dirs
mgimind doctor --fix  # Downloads embedding model, ONNX runtime, fixes issues
```

### Server deployment (Linux)

For running on a server (VPS/dedicated):
```bash
# 1. Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y

# 2. Clone and build
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind && cargo build --release

# 3. Start Qdrant (systemd or docker)
docker run -d --restart=always -p 6333:6333 -p 6334:6334 \
  -v /data/mgimind/qdrant:/qdrant/storage \
  --name mgimind-qdrant qdrant/qdrant

# 4. Initialize
./target/release/mgimind init
./target/release/mgimind doctor --fix

# 5. (Optional) Create systemd service for MCP server
```

## Your Protocol (MANDATORY)

### On Session Start
1. Run `mgimind session last` to read the previous session summary
2. Run `mgimind session start --agent <your-name>` to begin logging
3. Greet the user with context from the last session

> Always use the SAME `--agent <your-name>` for `start` and `end`. Sessions are
> per-agent, so concurrent agents never clobber each other's session (audit #14).

### During Session
- Before answering about past events, projects, or preferences: `mgimind search "<query>"`
- When user shares new information: `mgimind add <library> "<content>"`
- When user states a fact: `mgimind fact add "<subject>" "<predicate>" "<object>"`
- When user asks what you know: `mgimind fact query "<subject>"`
- Never guess - always check memory first

### Self-Configuration Protocol

MGI-Mind has a 4-tier memory hierarchy. Use the right tier for each type of information:

```
Tier 0: AI config file    -> permanent rules, identity, workflow    -> 0 tokens per query
Tier 1: KG facts          -> structured facts (user->prefers->Rust)   -> 10-50 tokens
Tier 2: Memories           -> details, context, notes               -> 100-500 tokens
Tier 3: Vault             -> secrets, passwords, keys               -> only on request
```

When the user establishes a PERMANENT rule or preference (examples: "always use Rust",
"never auto-commit", "I prefer dark themes", "my name is X"), do BOTH:

1. Store in MGI-Mind: `mgimind fact add "<subject>" "<rule>" "<value>"`
2. Suggest adding to the AI's own config file:

```
I've saved this to memory. I also recommend adding it to your AI config
file so every AI session knows this from the start:

  File: CLAUDE.md (or .cursorrules, .clinerules - depends on your AI tool)
  Add:  "Priority language: Rust for all new projects"

This way it's in the system prompt - zero lookup cost, always active.
Want me to add it? [y/N]
```

Config file locations by AI tool:
- Claude Code: `CLAUDE.md` in project root or `~/.claude/CLAUDE.md` globally
- Cursor: `.cursorrules` in project root
- Cline: `.clinerules` in project root
- Custom agents: wherever the system prompt is configured

Rules for self-configuration:
- ALWAYS ask permission before modifying config files
- Only suggest for PERMANENT preferences, not temporary context
- Show exactly what will be added before writing
- Never remove existing config - only append
- If unsure whether something is permanent, ask: "Should this be a permanent rule?"

### On Session End
- Run `mgimind session end --agent <your-name> --summary "<what was done, what's planned next>"`
- Keep summaries concise (under 200 words)

## Commands Reference

### Library Management
```bash
mgimind create <name>       # Create a new library (topic/project)
mgimind drop <name>         # Delete a library
mgimind list                # List all libraries
```

### Memory Operations
```bash
mgimind add <library> "<content>" [--source "<tag>"]
mgimind search "<query>" [--library <name>] [--limit 5] [--tier 2]
```

### Tiered Retrieval (Token Optimization)
- `--tier 1`: Facts only, ~100 chars max. Use for quick lookups.
- `--tier 2`: Summaries, ~500 chars. Default. Good balance.
- `--tier 3`: Full content. Use only when detail is needed.

Always start with tier 1 or 2. Only escalate to tier 3 if needed.

### Knowledge Graph
```bash
mgimind fact add "<subject>" "<predicate>" "<object>"
mgimind fact query "<subject>"
mgimind fact invalidate "<id>"
```

### Session Management
```bash
mgimind session start --agent <name>
mgimind session last [--agent <name>]
mgimind session end --agent <name> --summary "<text>"
```

### Secure Vault (passwords, SSH, API keys)
```bash
mgimind vault store <key> <value> --category ssh --desc "My server"
mgimind vault get <key>           # TERMINAL ONLY: prompts for master password (hidden) + confirm
mgimind vault list                # Shows keys only, never values
mgimind vault delete <key>
```

IMPORTANT: Vault is separate from regular memory. Secrets never appear in search results.
When user asks to store a password/key/token, use vault, NOT add.
The vault is **terminal-only**: the master password and decrypted secrets are NEVER
passed through MCP/the model channel. Do not try to read a secret yourself — tell the
user to run `mgimind vault get <key>` in their terminal (the `mind_vault_get` MCP tool
returns these instructions, not the secret).

### Qdrant Management
```bash
mgimind serve       # Start bundled Qdrant server
mgimind stop        # Stop Qdrant server
```

### Statistics
```bash
mgimind stats       # Show counts: memories, facts, sessions, vault entries
```

### Maintenance
```bash
mgimind doctor [--fix]      # Health check, auto-download dependencies
mgimind backup <file>       # Backup all data
mgimind restore <file>      # Restore from backup
mgimind export --format json [--output <dir>]
```

## Safe Disconnection

1. Call `mgimind session end --summary "..."` with a final summary
2. Remove the MCP configuration entry pointing to mgimind
3. Data remains safe on disk

## Bundled Tools

MGI-Mind ships with integrated tools. These are separate MCP servers that work alongside mgimind.

### CRW - Web Reader (Rust)

Read any web page and convert it to clean Markdown for AI consumption.
Bypasses most bot protection (Cloudflare, etc.) automatically.

Install: `cargo install crw-mcp crw-cli`

MCP config (add alongside mgi-mind):
```json
{
  "mcpServers": {
    "crw": {
      "command": "crw-mcp"
    }
  }
}
```

CLI usage:
```bash
crw "https://example.com"            # Returns clean Markdown
crw "https://example.com" --json     # Returns structured JSON
```

Use CRW when:
- User asks to read a webpage, docs, or article
- You need to check current information online
- Building context from external sources
- Importing web content into MGI-Mind memory

Workflow: read page with CRW -> save relevant parts with `mgimind add`

### Import from Obsidian / Markdown

```bash
mgimind import obsidian /path/to/vault --library notes
mgimind import markdown /path/to/folder --library docs
```

Scans .md files recursively, chunks into ~500 char segments, embeds and stores.
Skips hidden directories (.obsidian, .trash). Deduplicates automatically.

## Data Location

```
~/mgimind/
  config.json          # Configuration
  vault.enc            # AES-256-GCM encrypted secrets
  vault.salt           # Argon2 salt for key derivation
  sessions/            # Session logs
  models/              # Embedding model files (ONNX)
  qdrant/              # Vector database storage
```

User owns all data. User can move, backup, delete at any time.

## Important Rules

1. NEVER store secrets in regular memory - use `vault store` instead
2. ALWAYS confirm before dropping a library (destructive)
3. PREFER tier 1-2 searches to minimize token usage
4. LOG every session - continuity depends on it
5. VERIFY facts before stating them - search first, answer second
6. NEVER read vault secrets yourself — direct the user to `mgimind vault get` in a terminal (secrets don't cross the MCP channel)
7. Use CRW to read web pages - do NOT hallucinate content from URLs
8. After reading a web page, offer to save key info to MGI-Mind memory

---
MGI-Mind v0.2.0 | Apache 2.0 | Mad God Inc
