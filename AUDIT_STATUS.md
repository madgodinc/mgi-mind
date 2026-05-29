# Audit Status — MGI-Mind v0.2.0

This document accounts for **every** item in the security/quality audit (27 issues
across Parts I–III, plus the Part II competitive roadmap). Each row is either
**fixed in v0.2** (with where) or **deferred to v0.3** (with why). Nothing is
silently dropped.

Legend: ✅ done in v0.2 · 🟡 partial (mechanism in place, hardening continues) · 🔜 deferred to v0.3 (with rationale)

## Part I — Security & data loss (🔴)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 1 | No tests | ✅ | `cargo test` suite added (`util`, `storage`, `vault`, `config`, `cli` unit tests, 12 tests). GitHub Actions CI (`.github/workflows/ci.yml`): fmt + clippy `-D warnings` + test + `cargo audit`. |
| 2 | Vault broken/insecure over MCP | ✅ | Master password read via `rpassword` (needs a TTY) — over a non-interactive MCP channel it errors instead of silently using an empty password. `mind_vault_get` no longer returns plaintext over MCP; it instructs the user to run `mgimind vault get` in a terminal. |
| 3 | Master password not masked | ✅ | `rpassword::prompt_password` (no echo). Password buffers `zeroize()`d after key derivation. |
| 4 | Non-atomic file writes → corruption | ✅ | `util::atomic_write` (temp file + fsync + rename) used for config, vault, salt, sessions, exports. |
| 5 | `retrieve()` rewrites the whole vault on read | ✅ | Reads are read-only now; `last_accessed` is no longer written on `get`. |
| 6 | Downloaded binaries unverified | 🟡 | `util::download_file` verifies SHA-256 fail-closed. Pinned hashes for linux-x64 ONNX Runtime, Qdrant, and the default model (`integrity.rs`). Other platforms/custom models download with an explicit "integrity not verified" warning (pin them in `integrity.rs`). |
| 7 | Qdrant no auth + plaintext-bound | ✅ | `mgimind serve` binds `QDRANT__SERVICE__HOST=127.0.0.1` (loopback only) and sets `QDRANT__SERVICE__API_KEY` when `qdrant_api_key` is configured; the client authenticates with it. |

## Part I — Correctness (🟠)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 8 | Incoherent dedup logic | ✅ | Removed the `score > 0.99 && hash ==` check entirely. Replaced by content-addressed IDs (see #15) — identical content is an idempotent upsert. |
| 9 | `history()` doesn't sort | ✅ | Sorted by `created_at` descending (RFC3339 sorts chronologically). |
| 10 | `export` silently caps at 10k | ✅ | `scroll_all()` paginates via `next_page_offset` to the end. |
| 11 | Vector size hardcoded (384) | 🟡 | `vector_size` is in config; collections use it; `check_dim()` validates every embedding (memory **and**, since 0.2.1, facts) against it. 0.2.1 also adds a config↔collection dimension check at `serve` startup (`dimension_mismatches`, best-effort warn). Still no automatic re-index on a model swap — that migration is a deploy-time step. |
| 12 | KG query loses data (top-20 + substring) | ✅ | `query_facts` scrolls the full facts collection and filters by subject **or** predicate **or** object; valid-only. |
| 13 | `invalidate` hard-deletes; `valid` unused; no fact dedup | ✅ | `invalidate_fact` now `set_payload valid=false` (soft delete, honored by queries). Facts get deterministic IDs from `(s,p,o)` → dedup. |
| 14 | Session model breaks under concurrency | ✅ | Removed the single global `.current`; per-agent `.current.<agent>` pointers. Filenames carry seconds + a random suffix (no same-minute collision). `session end`/`last` are scoped by `--agent`. (0.2.1: the agent-name `sanitize` is now injective `_HH` escaping — a review found the original `_`-for-everything mapping let `team a`/`team/a`/`team.a` collapse to one pointer and re-clobber.) |
| 15 | TOCTOU race in `add_memory` | ✅ | Deterministic `UUIDv5(namespace, library + content)`: same content → same point ID → idempotent upsert. No read-before-write, no race, no duplicate. |

## Part I — Architecture & performance (🟡)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 16 | MCP spawns a process per call → model reloads | ✅ (0.3.0) | `mgimind daemon` (`src/daemon.rs`) loads the ONNX session + tokenizer once and serves newline-JSON requests over a Unix socket (`~/mgimind/daemon.sock`); the MCP client (`mcp-server/index.js`) routes embed-heavy calls (search/add/fact_add/context/history/stats) to it and **falls back to spawning the CLI** if the socket is absent — so the daemon is a pure optimization, never required. Runtime-validated against live data: warm add 31ms vs cold CLI 175ms (~5.6×; the audit's "2–5s" applies to a cold-disk/first load — the model is normally page-cached). **Operational steps remaining:** autostart entry + cutover of the live instance. |
| 17 | Tokenizer re-read from disk every embed | ✅ | Tokenizer cached in a `OnceCell`, loaded once (like the ONNX session). |
| 18 | Cross-library search is sequential | ✅ (0.4.0) | Single `memories` collection with a `library` payload field + keyword index; search runs one query (global top-k, or a `library` filter) instead of scanning N collections and merging. A `created_at` datetime index powers `history` via `order_by` (this also fixes the post-0.2 review's O(total) `history` finding — newest-N without scrolling everything). `mgimind migrate [--purge]` imports legacy `mem_*` collections (re-embeds from stored content, preserves `created_at`, idempotent). Runtime-validated on an isolated instance: global+filtered search, ordered history, per-library counts, drop-by-filter, and migrate all verified. |
| 19 | Heavy shellouts (curl/tar/unzip) | ✅ | Native `reqwest` downloads; native `flate2`+`tar` and `zip` extraction; native gzip+tar backup/restore. No external `curl`/`tar`/`unzip` needed. (`crw` web reader stays optional/external.) |
| 20 | Naive `chunk_text` | 🟡 | Overlap between chunks + hard-split of overlong single lines (no more giant chunks). Token-aware / tree-sitter (AST) chunking deferred to v0.3. |

## Part I — Search quality (🔵)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 21 | Weak/old embedder for code | ✅ (0.5.0) | Default model is now **multilingual-e5-base** (768-dim) — a big RU/EN upgrade over English-only MiniLM, CPU-practical (quantized ONNX, 266 MB). Embedder made architecture-flexible (mean/cls pooling, optional token_type_ids, query/passage prefixes; pooling unit-tested). Owner chose e5-base over bge-m3 (bge-m3 ONNX is 6.8 GB, too heavy GPU-less). **Runtime-validated on an isolated instance with the real model**: RU queries returned the correct top result every time (e.g. «искусственный интеллект для трансляций» → Aurora 0.79; «что приготовить на обед» → борщ 0.82, not the tech entries). **Operational step remaining:** live cutover (`doctor --fix` + `migrate` to re-embed existing memories at 768-dim). |
| 22 | No reranker | 🔜 v0.3 | Cross-encoder rerank over top-k — ships with the code-embedder work. |
| 23 | No hybrid (keyword/BM25) search | 🔜 v0.3 | Needs sparse vectors / full-text payload index + RRF fusion; pairs with the single-collection redesign (#18). |
| 24 | Tiers blind-truncate by chars | 🟡 | Truncation now stops on a word boundary (no mid-token cuts). Precomputed summaries are a v0.3 item. |

## Part I — Maturity (⚪)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 25 | Dependencies on the edge (`ort` rc) | 🟡 | `cargo audit` runs in CI. `ort` stays on the latest rc (no stable release yet); pin when one lands. `Cargo.lock` committed. |
| 26 | `vault.count` metadata leak | ✅ | Removed the plaintext counter file. `stats`/`context` show `initialized (locked)` / `empty` instead of a count. |
| 27 | README diverges from reality | ✅ | README/AI_INSTRUCTIONS corrected (masking, chronological history, dedup semantics, "triple store", session protocol). This file + `CHANGELOG.md` document actual behavior. |

## Part II — Competitive roadmap (additions, not bugs) — 🔜 v0.3+

Tracked, not implemented in v0.2 (these are net-new features, intentionally staged
after the core is solid): Д1 bench harness (LoCoMo/LongMemEval), Д2 auto-extraction
ingest pipeline, Д3 bi-temporal facts, Д4 decay/consolidation, Д5 self-editing memory,
Д6 procedural memory, Д7 multi-tenant shared memory. The single-collection + daemon
foundation (#16, #18) is the prerequisite for most of these.

## Part III — Code-verification gate — 🔜 separate component

A standalone verification service (G1 static → G2 tests → G3 property/fuzz → G4
sanitizers; AI as fixer, not judge; sandboxed execution). Out of scope for the memory
core; planned as a sibling project.

---

**Summary (counted per issue):** of the 27 audited issues, **20 are fully fixed**
(#1–5, 7–10, 12–19, 21, 26, 27), **5 are partial** (#6, #11, #20, #24, #25 — mechanism
shipped, hardening/deploy continues), and **2 are deferred** (#22 cross-encoder
reranker, #23 hybrid/BM25 search). 0.3.0 closed the daemon (#16); 0.4.0 closed the
single-collection redesign (#18); 0.5.0 closed the multilingual embedder (#21,
e5-base, runtime-validated — only the live cutover/re-embed remains operationally).
What remains is the reranker (#22) and hybrid search (#23); since e5 is dense-only,
#23 needs a separate sparse path (where bge-m3 would have given sparse for free —
traded away for CPU practicality).

A post-0.2.0 code review (recorded separately) confirmed the ✅ rows hold up in the
source, and surfaced regressions introduced by the fixes themselves — a `sanitize`
collision (re-opened #14), `created_at` being reset on re-add, and an unguarded
facts path (#11). Those are fixed in **0.2.1** (see `CHANGELOG.md`). The remaining
known limitation is `history` being O(total memories); its `order_by` fix rides with
the v0.3 storage rework.
