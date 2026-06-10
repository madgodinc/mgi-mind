# MGI-Mind

**[English](README.md)** | **[Русский](README.ru.md)** | **[中文](README.zh.md)**

**[Latest release: v1.6.4](https://github.com/madgodinc/mgi-mind/releases/tag/v1.6.4)** · **[CHANGELOG](CHANGELOG.md)** · **[Discussions](https://github.com/madgodinc/mgi-mind/discussions)** · **[Issues](https://github.com/madgodinc/mgi-mind/issues)** · **[Contributing](CONTRIBUTING.md)**

<p align="center">
  <img src="docs/brain-demo.gif" alt="Memory visualized as a brain — glowing cores wired by neurons" width="760">
  <br>
  <em>Your memory as a brain — cores (memories, facts, regions) wired by neurons, with live pulses. Run <code>mgimind brain</code> or just ask your assistant to show it.</em>
</p>

Local long-term memory for AI assistants. One Rust binary, a local Qdrant
vector database, local ONNX models. Speaks MCP, so Claude Code and other
assistants read and write memory on their own. Also a normal CLI.

```
You:  what was the deploy server again?

Assistant (calls mind_search "deploy server"):
  -> "Deploy server is 10.0.0.5:8080, SSH as deploy@, key in vault"  (source: infra.md)

You:  right, thanks
```

Nothing leaves the box — embeddings, search, reranking, vault are all
local. No cloud account, no API key, no telemetry.

---

## Contents

- [What it is](#what-it-is)
  - [The validity model](#the-validity-model)
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

MGI-Mind sits between you and your assistant. The assistant writes short
notes ("memories") and facts as a conversation goes, and pulls the
relevant ones back when they matter.

Retrieval, not just storage:

- **Hybrid search.** Each memory is stored as two vectors: a dense vector
  (multilingual-e5-base, 768 dims) for meaning, and a sparse term-frequency
  vector (TF-IDF, BM25-style) for exact words. A query runs both arms and
  fuses them with Reciprocal Rank Fusion. "The server box" finds "deploy
  host" through the dense arm; `fossilize_replay` finds the one note that
  contains that exact token through the sparse arm.
- **Cross-encoder reranking.** Fused top candidates are re-scored by
  bge-reranker-base, which reads the query and each passage together and
  is more accurate than comparing vectors. On by default, English-tuned —
  see [Languages and the reranker](#languages-and-the-reranker) for the
  trade-off on other languages.
- **One warm process.** `mgimind mcp` is the MCP server itself: it runs
  for the whole session, models load once and stay warm in memory, so a
  lookup costs milliseconds instead of reloading on every call.

Around retrieval there's a knowledge graph for structured facts, per-agent
session logs for cross-session continuity, and an encrypted terminal-only
vault for secrets.

### The validity model

Hybrid search is table stakes; most memory tools have some version of it.
The part that is harder to find is what keeps the store from ossifying as it
fills with old, contradictory, or self-reinforcing beliefs. These run mostly
on their own:

- **Duel rule.** When a new fact contradicts an existing one on the same
  subject and predicate, the second write resolves against the first instead
  of piling up a second "truth". An entrenched fact (many dependants,
  confirmations, age) holds; a strong fresh fact flips it and dampens the
  loser to a hidden `stale` status; a borderline one is marked contested or
  diverted to quarantine. Nothing is deleted, so the audit log keeps the
  loser. Automatic, inside a normal `mind_fact add`.
- **Doubt window.** An entrenched fact has to keep re-justifying itself. A
  retrieval whose context has drifted from where the fact was learned does
  not strengthen it, and after enough such drifted retrievals the fact's
  ranking weight is halved until a fresh in-context confirmation. A
  background pass re-tests entrenched facts that have gone quiet, under hard
  guarantees: never during a tool call, a per-tick cap, and a load-aware
  cadence.
- **Inheritance discount.** Facts carried into a session from memory count at
  half weight and cannot co-confirm each other. One stale source agreeing
  with itself is not two confirmations, so memory can't self-reinforce into
  false certainty.
- **Bi-temporal facts.** A predicate can be registered (`mind_predicate`) as
  single-valued, temporal-single (one current value, but the previous ones
  are kept and queryable by date via `mind_history`), or multi-valued. A
  superseded value is hidden from default ranking, not erased.
- **Typed outcome signals.** `mind_outcome` records that a remembered fix
  actually worked (`test_passed`, `code_compiled`, `user_confirmed`,
  `cited_by`). A real success raises a memory's weight and marks a procedure
  verified; a failure pulls the weight down rather than being ignored.

These are research-shaped mechanisms, and their tuning constants are still
being calibrated (see [BENCHMARKS.md](BENCHMARKS.md) for what is and isn't
measured). The shape is the point: a memory that argues with itself and
demotes what stops holding up, instead of accreting forever.

## Why bother

An assistant without memory asks for the same context every session and
can't build on yesterday's work. The usual workaround is you keeping
notes, tags, and folders — which the assistant still can't read by
meaning.

The thing MGI-Mind does that Obsidian and Notion don't: **the system
decides what to write down, you don't.** The MCP server reads what the
assistant is doing in real time and routes facts, decisions, and fixes
into the store through a relevance gate. You don't file, you don't tag,
you don't decide what's worth saving. Low-signal candidates land in a
quarantine layer (recoverable on re-assertion) instead of polluting
retrieval.

How it compares to the obvious alternatives:

- **Plain notes (Obsidian, Notion).** Strong as your personal notebook,
  but the assistant can't search them by meaning, and every keystroke is
  equally important to a folder of `.md` files.
- **Bare vector database.** Semantic search, but no exact-term matching,
  no reranking, no dedup, no sessions, no facts, no secrets handling, no
  relevance gate, no procedural memory. You assemble all of that.
- **Hosted memory API.** Your data on someone else's servers. Closes the
  box on inspection, dedup behaviour, and what the relevance gate is
  actually doing.

MGI-Mind is the assembled local version: hybrid + reranked retrieval, the
relevance gate, dedup, facts, sessions, procedural memory ("error → fix"
playbooks), and a terminal-only vault — behind one binary you run
yourself.

A note on evaluating it. [BENCHMARKS.md](BENCHMARKS.md) reports **retrieval
recall** (R@k on LongMemEval-S, zero-API, no LLM judge): given a question,
did the gold session land in the top-k. On the default CPU config that is
R@5 ≈ 98%. This is not the same metric as the QA-accuracy numbers other
memory tools publish (they run an answerer and a judge over their retrieval),
so do not put it in the same table. It measures whether the evidence was
retrievable, not whether an LLM then answered correctly. mgi-mind has no
answering step, so retrieval recall is the number it actually owns.

The honest gap: the validity model (duel rule, doubt window, supersession)
is the differentiator, and retrieval recall does not test it. The benchmark
that does is STALE, a belief-revision suite. A preliminary partial run exists
(N=155, ~32%, raw verdicts committed under `benchmark/results/`), but it was
produced by the harness on a branch rather than the scaffold on `main`, used a
reduced haystack, and may have run while the duel rule was still broken. See
[BENCHMARKS.md](BENCHMARKS.md) for the full caveats. Treat the validity model as
a designed mechanism with a preliminary, not-yet-clean score, not a proven win.

## Quick start

One command. The installer drops the binary on PATH and runs `init` +
`doctor --fix` (which pulls Qdrant, ONNX Runtime, and the models).

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.sh | sh
```

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.ps1 | iex
```

When it finishes, the printed command wires the server into Claude Code:

```bash
claude mcp add mgimind -- /home/you/.local/bin/mgimind mcp
```

`mgimind mcp` IS the MCP server; it runs for the whole session with the
models warm and brings up the bundled Qdrant on first use. Point your
assistant at [`AI_INSTRUCTIONS.md`](AI_INSTRUCTIONS.md) once so it knows
the protocol (log a session, search before answering, use the vault for
secrets).

`doctor --fix` downloads into `~/mgimind/`: ONNX Runtime (the library the
embedder loads), the Qdrant binary, the embedding model
(multilingual-e5-base, quantized ONNX, ~270 MB) and the reranker
(bge-reranker-base, quantized ONNX, ~280 MB).

### Installer flags

- `INSTALL_DIR=/opt/mgimind curl ... | sh` — install somewhere other than `~/.local/bin`.
- `MGIMIND_TAG=v1.6.4 curl ... | sh` — pin a specific release instead of `latest`.
- `SKIP_DOCTOR=1 curl ... | sh` — just drop the binary; run `init` + `doctor --fix` yourself later.

### Manual install (no installer)

If you'd rather not pipe a script to a shell, grab the release tarball
for your OS from [Releases](https://github.com/madgodinc/mgi-mind/releases/latest),
put `mgimind` on your PATH, then:

```bash
mgimind init
mgimind doctor --fix
claude mcp add mgimind -- /absolute/path/to/mgimind mcp
```

Try it from the CLI (the same binary works without an assistant):

```bash
mgimind create work
mgimind add work "Deploy server is 10.0.0.5:8080, SSH as deploy@"
mgimind search "how do I reach the deploy box"
```

### Per-OS notes

- **Linux x86_64, macOS arm64 (Apple Silicon), Windows x86_64** — prebuilt
  binaries in every release. The installer picks the right one.
- **macOS Intel (x86_64)** — no prebuilt binary. GitHub's hosted
  `macos-13` runner sits in queue for 20-30+ minutes and is being phased
  out, so it's omitted from the release matrix. Build from source (next
  section); takes a few minutes.
- **macOS first-run quarantine** — a downloaded binary may need
  `xattr -d com.apple.quarantine /path/to/mgimind`, or right-click → Open
  once in Finder.
- **Windows** — SmartScreen may warn on the unsigned `mgimind.exe`
  ("Windows protected your PC"); choose **More info → Run anyway**.
  Antivirus can also quarantine the binary or the models it downloads; if
  `mgimind doctor` reports a file as downloaded but missing, allow
  `mgimind.exe` and the `%USERPROFILE%\mgimind` folder in your AV, then
  re-run `mgimind doctor --fix`. Code signing to remove the SmartScreen
  prompt is on the roadmap.

### Build from source

Rust toolchain (`rustup`); no other dependencies.

```bash
git clone https://github.com/madgodinc/mgi-mind.git
cd mgi-mind
cargo build --release                  # binary: target/release/mgimind
```

Then run `target/release/mgimind init && target/release/mgimind doctor --fix`
and wire it in with
`claude mcp add mgimind -- /absolute/path/to/target/release/mgimind mcp`.

## Using it

**By hand (CLI):**

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

**Duel rule on contradictions.** When two facts contradict and the
predicate is `Single` or `TemporalSingle`, the second add resolves
against the first instead of piling up:

```bash
# Tell the system this predicate has one current value at a time:
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call",
  "params":{"name":"mind_predicate","arguments":
    {"action":"register","predicate":"lives_in",
     "cardinality":"TemporalSingle"}}}' \
  | mgimind mcp

mgimind fact add "Alice" "lives_in" "Prague"
mgimind fact add "Alice" "lives_in" "Dublin"
mgimind fact query "Alice"
#   Alice -> lives_in -> Dublin
# (Prague is preserved as history, queryable via mind_history / audit log
#  but hidden from default ranking.)
```

If you migrated from a pre-v1.7 install where the duel rule wasn't
firing at the read path, run `mgimind migrate-v14 redo-duels --apply`
once to collapse legacy contradictions to canonical answers. The
walk is idempotent and dry-run by default.

**Through your assistant (MCP).** Once connected, you just talk:

```
You:  remember that the staging DB password is in the vault under "staging-db"
Assistant: (mind_add) saved. The secret itself stays in your terminal vault, not here.

You:  what database are we using on staging?
Assistant: (mind_search "staging database") Postgres 16, host db-staging.internal:5432
```

Search returns results in tiers so the assistant spends tokens carefully:
`--tier 1` is a ~100-character snippet, `--tier 2` (default) is ~500,
`--tier 3` is the full text.

## How it works

```
  your AI assistant
        |  MCP (JSON-RPC over stdio)
        v
  mgimind mcp  (one Rust process: MCP server + embedder, models stay warm)
        |  starts on first use
        v
  Qdrant (local, loopback only)
        |
        one "memories" collection, two vectors per point:
        dense (e5, meaning) + sparse (TF-IDF, exact terms)
```

**Storage.** All memories live in one Qdrant collection. A `library` field
on each point separates namespaces (work, personal, a project), and a
query can filter to one library or search across all. A point's ID is a
UUIDv5 of `library + content`, so adding the same text twice overwrites
the same point — no duplicates, no race. A `created_at` datetime index
lets `history` return the newest N directly without scanning.

**Embeddings.** Text is embedded locally through ONNX Runtime. Default is
multilingual-e5-base (768 dimensions), strong on English and handles
mixed languages. The embedder is model-aware: pooling (mean or CLS),
`token_type_ids` input, query/passage prefixes are all config, so
switching models doesn't need a code change. Inputs cap at 512 tokens;
`add` splits long text into chunks so nothing past the cap silently
disappears.

**Search.** The query is embedded once. Qdrant runs a dense nearest-
neighbor search and a sparse search in a single Query API call and fuses
them with RRF. If reranking is on, the top `rerank_top_k` candidates are
re-scored by the cross-encoder and reordered. A `library` filter, when
given, applies to both arms.

**Safety.** Downloads check against pinned SHA-256 hashes (fail-closed).
Qdrant binds to loopback only and can require an API key. The vault is
terminal-only — the master password and decrypted secrets never travel
over the MCP channel. File writes are atomic (temp file, fsync, rename,
fsync the directory), so a crash leaves the old file or the new one,
never a corrupt one.

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
| `mgimind mcp` | Run as the MCP server over stdio (what your assistant connects to). One warm process; starts Qdrant automatically. |
| `mgimind serve` / `mgimind stop` | Start / stop the bundled Qdrant by hand (rarely needed — `mcp` does it for you). |
| `mgimind migrate [--purge]` | Re-embed legacy per-library collections into the single `memories` collection. Idempotent. `--purge` deletes the old collections afterward. |
| `mgimind backup <file>` / `mgimind restore <file>` | gzip+tar of the whole data directory. |
| `mgimind export [--format json\|md] [--output <dir>]` | Export memories to files. |
| `mgimind import <obsidian\|markdown> <path> [--library <l>]` | Import a folder of markdown (recursively, chunked). |
| `mgimind doctor [--fix]` | Health check; `--fix` downloads anything missing. |

## Configuration

Config lives at `~/mgimind/config.json`. Fields that affect retrieval:

| Field | Default | Meaning |
|---|---|---|
| `model_name` | `multilingual-e5-base` | Embedding model directory under `models/`. |
| `vector_size` | `768` | Embedding dimension. Must match the model. |
| `pooling` | `mean` | `mean` (e5, MiniLM) or `cls` (some XLM-R models). |
| `uses_token_type_ids` | `false` | `true` for BERT-family models, `false` for XLM-R / e5. |
| `query_prefix` / `passage_prefix` | `query: ` / `passage: ` | e5 needs these; empty for models that don't. |
| `rerank_enabled` | `true` | Cross-encoder reranking. See the language note below. |
| `rerank_model` | `bge-reranker-base` | Reranker directory under `models/`. |
| `rerank_top_k` | `20` | How many candidates to fetch and rerank before returning `limit`. |
| `qdrant_port` | `6334` | Qdrant gRPC port. |
| `qdrant_api_key` | none | If set, Qdrant starts with it and the client authenticates. |

## Languages and the reranker

The default stack is tuned for English, because that's where the assistant
itself reasons best and where the models are strongest. It's the
recommended setup for an English-first project.

A few honest details:

- The embedder (multilingual-e5-base) is genuinely multilingual. An
  English query finds a Russian note and vice versa, and search alone
  works well across languages.
- The default reranker (bge-reranker-base) is English-tuned. It improves
  English ranking, but it **lowers ranking quality on Russian** (and
  other non-English languages). This is the one place the defaults favor
  English.
- **If your content is mostly Russian (or another non-English language):**
  set `rerank_enabled = false`. Hybrid dense+sparse search on its own
  ranks those languages well; the English-tuned reranker is what hurts
  them. Or swap in a stronger multilingual reranker.
- Reranking costs cross-encoder inference per query — roughly one to two
  seconds for 20 candidates on a CPU-only box. Lower `rerank_top_k` or
  turn reranking off for snappier search.

## Changing the embedding model

Switching models usually changes the vector dimension, so existing
memories must be re-embedded:

1. Back up first: `mgimind backup ~/mgi-backup.tar.gz`.
2. Set `model_name`, `vector_size`, `pooling`, `uses_token_type_ids`, and
   the prefixes in `config.json` for the new model.
3. `mgimind doctor --fix` to download it. Only the bundled defaults are
   pinned; a custom model downloads with an "integrity not verified"
   warning, so pin its SHA-256 in `integrity.rs` if you want strict
   verification.
4. `mgimind migrate` to re-embed everything from the stored text under
   the new model.

## Troubleshooting

- **"Model not found ... run doctor --fix"** — the model is not in
  `~/mgimind/models/`. Run `mgimind doctor --fix`.
- **"invalid expand shape" / inference errors** — usually an input far
  over 512 tokens. `add` chunks automatically; if you call the library
  directly, chunk first.
- **Searches are slow** — that's the reranker on CPU. Lower
  `rerank_top_k`, or set `rerank_enabled = false`. Models stay warm for
  the life of the `mgimind mcp` process, so only the first lookup of a
  session pays the load cost.
- **A tool fails right after install** — run `mgimind doctor` (the
  assistant can call `mind_doctor`); it reports exactly what's missing
  (Qdrant not running, a model not downloaded, ONNX Runtime absent, a
  file quarantined by AV) and `--fix` downloads what it can.
- **Dimension mismatch warning** — a collection's vectors don't match
  `vector_size`, usually after a model change. Re-embed with
  `mgimind migrate`.
- **Russian results feel off** — set `rerank_enabled = false` (see the
  language note above).

## Security

- Downloads verify against pinned SHA-256 (fail-closed) for ONNX Runtime
  (linux-x64), Qdrant, and the default models (e5 and the reranker).
  Other platforms and custom models warn instead of trusting blindly.
- Qdrant binds to `127.0.0.1` only and supports an API key.
- The vault is AES-256-GCM with an Argon2id-derived key (parameters
  pinned so a library upgrade can't lock you out). Terminal-only — the
  master password and decrypted secrets never travel over the MCP
  channel. The `mind_vault_*` MCP tools return terminal instructions,
  never the secret value.
- File writes are atomic and directory-fsynced — a crash leaves the old
  file or the new one, never a corrupt one.

## Status and audit

Current version: **1.6.4** (semver-stable since v1.0.0). The 0.x line built
the foundation: the audit log and ephemeral viewer (0.10), the quarantine
layer and best-effort retrieval policy (0.11), the viewer wave (0.12),
session liveness (0.13), and procedural memory (0.14, benchmarked on
LongMemEval-S plus a 227-pair error→fix dataset from 20 public repos). The
1.x line added the validity model: the duel rule, the doubt window and its
background re-test, bi-temporal fact supersession, the confidence score, and
typed outcome signals (`mind_outcome`), plus install-mode CPU/GPU profiles.

The MCP surface is a frozen contract until a 2.0 bump: 25 consolidated tools
plus 15 deprecated aliases kept for compatibility. The other 1.0 contracts
are the asymmetric "Qdrant now → md says" reconcile diff and the
`MGIMIND_MODEL_VARIANT={cpu|gpu|auto}` switch.

The project went through a code audit; [`AUDIT_STATUS.md`](AUDIT_STATUS.md)
accounts for every issue one by one with where it was fixed or why it was
deferred. [`CHANGELOG.md`](CHANGELOG.md) has the per-release history (current
through v1.6.4, with a v1.7 candidate section), and
[`ROADMAP.md`](ROADMAP.md) names what is committed for the next releases and
which directions are still **candidate** at the v3.0 horizon.

## Project layout

```
src/
  cli.rs         command dispatch and output rendering
  storage.rs     Qdrant: single collection, hybrid search, history, stats, migrate, chunking
  embedder.rs    ONNX embedding (model-aware pooling, prefixes, 512-token cap)
  reranker.rs    cross-encoder reranking
  knowledge.rs   knowledge-graph facts + cardinality + supersession
  duel.rs        duel rule: resolve contradicting facts (flip / contested / quarantine)
  doubt.rs       doubt window + background active re-test of entrenched facts
  confidence.rs  per-fact confidence score (dependants / confirmations / signals)
  outcome.rs     typed external signals (test_passed, code_compiled, ...) into weight
  procedure.rs   procedural memory: learn / recall / outcome
  ingest.rs      auto-extract & ingest candidates
  relevance.rs   relevance gate (length, blacklists, decision markers, token novelty)
  retrieval_policy.rs  search-before-answer classifier
  provenance.rs  cited external snippets with mandatory source
  extractor.rs   optional local LLM extraction (off by default)
  consolidate.rs merge duplicates, report cold entries
  md_reconcile.rs md import as reconcile with "md wins"
  audit.rs       append-only audit log for every storage mutation
  http_api.rs    loopback HTTP surface for multi-agent access
  viewer.rs      ephemeral local HTTP viewer (axum, static frontend baked in)
  pulse.rs       live graph pulses for the viewer
  session.rs     per-agent session files
  secrets.rs     secret-scrub on the write path
  vault.rs       encrypted secret vault (terminal only)
  mcp.rs         MCP server over stdio (hand-rolled JSON-RPC; warm in-process models)
  config.rs      configuration + install-mode profiles
  integrity.rs   pinned SHA-256 hashes for downloads
  util.rs        atomic writes, verified downloads
  (plus migrate, install-mode, access/decay, and bench harnesses)
tests/
  cli_integration.rs   black-box tests against a real Qdrant (CLI + MCP round-trip)
  http_integration.rs  the multi-agent HTTP surface end to end
```

## License

Apache-2.0. See [`LICENSE`](LICENSE).
