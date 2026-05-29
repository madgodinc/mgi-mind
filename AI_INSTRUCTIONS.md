# MGI-Mind - AI Instructions

You are connected to MGI-Mind, a self-hosted long-term memory system. All data stays
on the user's machine. You are the interface to it. Read this file fully before you
start.

For install and architecture details, see [`README.md`](README.md). This file is the
operating protocol for you, the assistant.

## What you get

A local memory the user owns. You write to it as you work and read from it by meaning.
Retrieval is hybrid (dense e5 + sparse BM25, fused with RRF) and reranked by a
cross-encoder, so a search returns results by semantic relevance and exact-term match
at once. You do not manage any of that; you just call the tools below.

If a warm daemon is running (`mgimind daemon`), the MCP server uses it and lookups are
fast. If not, it falls back to spawning the CLI. Either way the commands are the same.

## Your protocol (mandatory)

### On session start
1. `mgimind session last` to read the previous session summary.
2. `mgimind session start --agent <your-name>` to begin logging.
3. Greet the user with context from the last session.

Always use the SAME `--agent <your-name>` for `start` and `end`. Sessions are
per-agent, so concurrent agents never overwrite each other's session.

### During the session
- Before answering about past events, projects, or preferences: `mgimind search "<query>"`.
- When the user shares something worth keeping: `mgimind add <library> "<content>"`.
- When the user states a fact: `mgimind fact add "<subject>" "<predicate>" "<object>"`.
- When the user asks what you know about something: `mgimind fact query "<term>"`.
- Do not guess from memory. Search first, answer second.

### On session end
- `mgimind session end --agent <your-name> --summary "<what was done, what is next>"`.
- Keep the summary under about 200 words.

## Where to put information

```
AI config file (CLAUDE.md, etc.)  permanent rules, identity, workflow   0 lookup cost
KG facts (fact add)               structured facts: subject-predicate-object
memories (add)                    details, notes, context
vault (vault store)               secrets: passwords, keys, SSH         terminal only
```

When the user sets a PERMANENT rule or preference (for example "always use Rust",
"never auto-commit", "my name is X"):
1. Store it: `mgimind fact add "<subject>" "<predicate>" "<value>"`.
2. Suggest adding it to the AI config file so every future session has it in the
   system prompt at zero lookup cost. Show exactly what you would add and ask first.

Config file by tool: Claude Code uses `CLAUDE.md` (project root or `~/.claude/`),
Cursor uses `.cursorrules`, Cline uses `.clinerules`. Never remove existing config,
only append, and always ask before writing.

## Commands

### Memory
```bash
mgimind add <library> "<content>" [--source "<tag>"]
mgimind search "<query>" [--library <name>] [--limit 5] [--tier 1|2|3]
mgimind history [--limit 10]
mgimind delete <library> <id>
mgimind context
```

Tier controls how much text comes back, not which results:
- `--tier 1`: about 100 chars per hit. Quick lookups.
- `--tier 2`: about 500 chars. Default.
- `--tier 3`: full content. Use only when you need the detail.

Start at tier 1 or 2; escalate to tier 3 only if needed.

### Libraries and stats
```bash
mgimind create <name>
mgimind list
mgimind drop <name>          # destructive: confirm with the user first
mgimind stats
```

### Knowledge graph
```bash
mgimind fact add "<subject>" "<predicate>" "<object>"
mgimind fact query "<term>"
mgimind fact invalidate "<id>"   # soft delete: kept, marked invalid
```

### Sessions
```bash
mgimind session start --agent <name>
mgimind session last [--agent <name>]
mgimind session end --agent <name> --summary "<text>"
```

### Vault (terminal only)
```bash
mgimind vault store <key> <value> --category ssh --desc "My server"
mgimind vault get <key>     # prompts for the master password in a terminal
mgimind vault list          # keys only, never values
mgimind vault delete <key>
```

The vault is separate from regular memory and secrets never appear in search results.
The master password and decrypted secrets NEVER cross the MCP channel. Do not try to
read a secret yourself. Tell the user to run `mgimind vault get <key>` in their
terminal. The `mind_vault_get` MCP tool returns these instructions, not the secret.
When the user wants to store a password, key, or token, use the vault, not `add`.

### Service and data
```bash
mgimind serve / mgimind stop      # bundled Qdrant
mgimind daemon                    # warm daemon (keeps models loaded)
mgimind migrate [--purge]         # import legacy per-library collections, re-embeds
mgimind doctor [--fix]            # health check; --fix downloads what is missing
mgimind backup <file> / mgimind restore <file>
mgimind export [--format json|md] [--output <dir>]
mgimind import obsidian /path/to/vault --library notes
```

`migrate` is for upgrading old data: it re-embeds entries from the previous
per-library layout into the single collection. It is idempotent, so it is safe to
re-run. Use it after a model change too (it re-embeds from stored text).

## Reading web pages (optional)

If the `crw` tool is installed, `mgimind web <url>` reads a page as clean Markdown.
Use it instead of guessing a page's content from its URL. After reading, offer to save
the useful parts with `mgimind add`.

## Data location

```
~/mgimind/
  config.json          configuration
  vault.enc            encrypted secrets
  vault.salt           Argon2 salt
  libraries.json       registered library names
  sessions/            session logs
  models/              ONNX models (embedder, reranker)
  qdrant/              vector database storage
```

The user owns all of this and can move, back up, or delete it at any time.

## Rules

1. Never store secrets in regular memory. Use the vault.
2. Confirm before dropping a library; it is destructive.
3. Prefer tier 1 or 2 searches; escalate only when needed.
4. Log every session. Continuity depends on it.
5. Verify before stating. Search first, answer second.
6. Never read vault secrets yourself. Direct the user to `mgimind vault get` in a terminal.
7. Do not hallucinate web content. Read the page first if `crw` is available.

---
MGI-Mind v0.7.x | Apache-2.0 | Mad God Inc
