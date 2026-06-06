# Audit Status - MGI-Mind v0.8.x

This document accounts for **every** item in the security/quality audit (27 issues
across Parts I-III, plus the Part II competitive roadmap). Each row is either
**fixed in v0.2** (with where) or **deferred to v0.3** (with why).

Legend: ✅ done in v0.2 · 🟡 partial (mechanism in place, hardening continues) · 🔜 deferred to v0.3 (with rationale)

## Part I - Security & data loss (🔴)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 1 | No tests | ✅ | 33 unit tests (util, storage, vault, config, mcp, integrity), including a vault encrypt/decrypt property roundtrip over varied payloads and the MCP protocol surface (tools/list, lifecycle, isError semantics), plus black-box integration tests (`tests/cli_integration.rs`) driving the built binary against a real Qdrant - over both the CLI and the `mgimind mcp` stdio transport (`mcp_add_then_search_roundtrip`). CI: fmt + clippy `-D warnings` + unit tests on Linux/macOS/Windows, an integration job against a Qdrant service container, and `cargo audit`. |
| 2 | Vault broken/insecure over MCP | ✅ | Master password read via `rpassword` (needs a TTY) - over a non-interactive MCP channel it errors instead of silently using an empty password. `mind_vault_get` no longer returns plaintext over MCP; it instructs the user to run `mgimind vault get` in a terminal. |
| 3 | Master password not masked | ✅ | `rpassword::prompt_password` (no echo). Password buffers `zeroize()`d after key derivation. |
| 4 | Non-atomic file writes → corruption | ✅ | `util::atomic_write` (temp file + fsync + rename) used for config, vault, salt, sessions, exports. |
| 5 | `retrieve()` rewrites the whole vault on read | ✅ | Reads are read-only now; `last_accessed` is no longer written on `get`. |
| 6 | Downloaded binaries unverified | 🟡 | `util::download_file` verifies SHA-256 fail-closed. **The full default stack is now pinned**: linux-x64 ONNX Runtime, Qdrant, the default embedder (multilingual-e5-base) and the reranker (bge-reranker-base) ONNX + tokenizers (`integrity.rs`). A 0.7.0 review caught that only the old MiniLM was pinned while the new defaults downloaded unverified; fixed in 0.7.2. Other platforms / custom models still download with an "integrity not verified" warning (pin them in `integrity.rs`). |
| 7 | Qdrant no auth + plaintext-bound | ✅ | `mgimind serve` binds `QDRANT__SERVICE__HOST=127.0.0.1` (loopback only) and sets `QDRANT__SERVICE__API_KEY` when `qdrant_api_key` is configured; the client authenticates with it. |

## Part I - Correctness (🟠)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 8 | Incoherent dedup logic | ✅ | Removed the `score > 0.99 && hash ==` check entirely. Replaced by content-addressed IDs (see #15) - identical content is an idempotent upsert. |
| 9 | `history()` doesn't sort | ✅ | Sorted by `created_at` descending (RFC3339 sorts chronologically). |
| 10 | `export` silently caps at 10k | ✅ | `scroll_all()` paginates via `next_page_offset` to the end. |
| 11 | Vector size hardcoded (384) | 🟡 | `vector_size` is in config; collections use it; `check_dim()` validates every embedding (memory **and**, since 0.2.1, facts) against it. 0.2.1 also adds a config↔collection dimension check at `serve` startup (`dimension_mismatches`, best-effort warn). Still no automatic re-index on a model swap - that migration is a deploy-time step. |
| 12 | KG query loses data (top-20 + substring) | ✅ | `query_facts` scrolls the full facts collection and filters by subject **or** predicate **or** object; valid-only. |
| 13 | `invalidate` hard-deletes; `valid` unused; no fact dedup | 🟡 | `invalidate_fact` now `set_payload valid=false` (soft delete, honored by queries) and facts get deterministic IDs from `(s,p,o)` (dedup). **Not done:** automatic supersession of single-valued predicates (e.g. `user lives_in X` then `... Y` leaves both valid). It is omitted deliberately - auto-superseding would corrupt multi-valued predicates (`user likes X`, `user likes Y`), which needs a per-predicate cardinality model. A 0.7.0 review flagged this as overclaimed; downgraded to partial. |
| 14 | Session model breaks under concurrency | ✅ | Removed the single global `.current`; per-agent `.current.<agent>` pointers. Filenames carry seconds + a random suffix (no same-minute collision). `session end`/`last` are scoped by `--agent`. (0.2.1: the agent-name `sanitize` is now injective `_HH` escaping - a review found the original `_`-for-everything mapping let `team a`/`team/a`/`team.a` collapse to one pointer and re-clobber.) |
| 15 | TOCTOU race in `add_memory` | ✅ | Deterministic `UUIDv5(namespace, library + content)`: same content → same point ID → idempotent upsert. No read-before-write, no race, no duplicate. |

## Part I - Architecture & performance (🟡)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 16 | MCP spawns a process per call → model reloads | ✅ (0.8.0) | Solved structurally: `mgimind mcp` (`src/mcp.rs`) **is** the MCP server, one Rust process for the whole session, so the ONNX session + tokenizer (global `OnceCell`) load once and stay warm - there is no per-call spawn to optimize away. This replaced the 0.3.0 Unix-socket daemon + Node client entirely (both removed), which also fixed the Windows build (no more `UnixListener`) and dropped the Node dependency. Warm vs cold remains ~5.6× (warm add 31ms vs cold CLI 175ms). |
| 17 | Tokenizer re-read from disk every embed | ✅ | Tokenizer cached in a `OnceCell`, loaded once (like the ONNX session). |
| 18 | Cross-library search is sequential | ✅ (0.4.0) | Single `memories` collection with a `library` payload field + keyword index; search runs one query (global top-k, or a `library` filter) instead of scanning N collections and merging. A `created_at` datetime index powers `history` via `order_by` (this also fixes the post-0.2 review's O(total) `history` finding - newest-N without scrolling everything). `mgimind migrate [--purge]` imports legacy `mem_*` collections (re-embeds from stored content, preserves `created_at`, idempotent). Runtime-validated on an isolated instance: global+filtered search, ordered history, per-library counts, drop-by-filter, and migrate all verified. |
| 19 | Heavy shellouts (curl/tar/unzip) | ✅ | Native `reqwest` downloads; native `flate2`+`tar` and `zip` extraction; native gzip+tar backup/restore. No external `curl`/`tar`/`unzip` needed. (`crw` web reader stays optional/external.) |
| 20 | Naive `chunk_text` | 🟡 | Overlap between chunks + hard-split of overlong single lines (no more giant chunks). Token-aware / tree-sitter (AST) chunking deferred to v0.3. |

## Part I - Search quality (🔵)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 21 | Weak/old embedder for code | ✅ (0.5.0) | Default model is now **multilingual-e5-base** (768-dim) - a big RU/EN upgrade over English-only MiniLM, CPU-practical (quantized ONNX, 266 MB). Embedder made architecture-flexible (mean/cls pooling, optional token_type_ids, query/passage prefixes; pooling unit-tested). Owner chose e5-base over bge-m3 (bge-m3 ONNX is 6.8 GB, too heavy GPU-less). **Runtime-validated on an isolated instance with the real model**: RU queries returned the correct top result every time (e.g. «искусственный интеллект для трансляций» → Aurora 0.79; «что приготовить на обед» → борщ 0.82, not the tech entries). **Operational step remaining:** live cutover (`doctor --fix` + `migrate` to re-embed existing memories at 768-dim). |
| 22 | No reranker | ✅ (0.6.0) | `src/reranker.rs`: cross-encoder **bge-reranker-base** (XLM-R, multilingual incl. RU; quantized ONNX 279 MB) scores each (query, passage) pair jointly in one padded batch, re-ordering the dense top-K. `search` fetches `rerank_top_k` (default 20) by dense, reranks, returns `limit`. Best-effort: any reranker failure leaves the dense order untouched. Runtime-validated: it sharply separates relevance (scores 1.07 / 0.83 / −0.66 where dense was a flat 0.86 / 0.84 / 0.82). Config: `rerank_enabled`/`rerank_model`/`rerank_top_k`; `doctor --fix` fetches the model. |
| 23 | No hybrid (keyword/BM25) search | ✅ (0.7.0) | The memories collection now carries **named vectors**: `dense` (e5 semantic, cosine) + `sparse` (BM25-style, IDF modifier server-side). `add_memory` writes both; `search` runs a Qdrant Query API with two prefetches (dense + sparse) fused by **RRF**, then reranks (#22). Sparse vectors are unicode-aware term-frequency (handles Cyrillic). Runtime-validated: exact rare terms (`fossilize_replay`, `gamemoderun`) are caught by the lexical arm while semantic queries ("как стим компилирует шейдеры") still hit via dense - both fused correctly. |
| 24 | Tiers blind-truncate by chars | 🟡 | Truncation now stops on a word boundary (no mid-token cuts). Precomputed summaries are a v0.3 item. |

## Part I - Maturity (⚪)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 25 | Dependencies on the edge (`ort` rc) | 🟡 | `cargo audit` runs in CI. `ort` stays on the latest rc (no stable release yet); pin when one lands. `Cargo.lock` committed. |
| 26 | `vault.count` metadata leak | ✅ | Removed the plaintext counter file. `stats`/`context` show `initialized (locked)` / `empty` instead of a count. |
| 27 | README diverges from reality | ✅ | README/AI_INSTRUCTIONS corrected (masking, chronological history, dedup semantics, "triple store", session protocol). This file + `CHANGELOG.md` document actual behavior. |

## Part II - Competitive roadmap (additions, not bugs) - 🔜 v0.3+

Tracked, not implemented in v0.2 (these are net-new features, intentionally staged
after the core is solid): Д1 bench harness (LoCoMo/LongMemEval), Д2 auto-extraction
ingest pipeline, Д3 bi-temporal facts, Д4 decay/consolidation, Д5 self-editing memory,
Д6 procedural memory, Д7 multi-tenant shared memory. The single-collection + warm
in-process MCP foundation (#16, #18) is the prerequisite for most of these.

## Part III - Code-verification gate - 🔜 separate component

A standalone verification service (G1 static → G2 tests → G3 property/fuzz → G4
sanitizers; AI as fixer, not judge; sandboxed execution). Out of scope for the memory
core; planned as a sibling project.

---

**Summary (counted per issue):** of the 27 audited issues, **21 are fully fixed**
(#1-5, 7-10, 12, 14-19, 21, 22, 23, 26, 27), **6 are partial** (#6 supply chain -
default stack pinned, custom/other-platform still warn; #11 Argon2 pinned, no
versioned re-derivation; #13 fact dedup + soft-delete, no supersession; #20 char
chunking, not token/AST-aware; #24 word-boundary truncation, no precomputed
summaries; #25 `ort` on rc), and **0 are deferred**. 0.3.0 daemon (#16, later
superseded), 0.4.0 single-collection (#18), 0.5.0 e5 embedder (#21), 0.6.0 reranker
(#22), 0.7.0 hybrid search (#23), 0.7.1 sequence-length + resilient migrate, 0.7.2
code-review fixes (pins, chunking, vault durability, tests), 0.8.0 single
cross-platform `mgimind mcp` binary replacing the daemon + Node stack (#16).

**Honest gaps a reviewer should know:** fact supersession is not implemented (#13);
the facts collection stores an unused dense vector; embedding is not batched; the MCP
server handles requests sequentially (one stdio client, so no concurrency - a session
pool is deferred until it serves many clients). **Operationally remaining** (not audit
items): the live cutover (deploy, `doctor --fix`, `migrate`).

A post-0.2.0 code review (recorded separately) confirmed the ✅ rows hold up in the
source, and surfaced regressions introduced by the fixes themselves - a `sanitize`
collision (re-opened #14), `created_at` being reset on re-add, and an unguarded
facts path (#11). Those are fixed in **0.2.1** (see `CHANGELOG.md`). The remaining
known limitation is `history` being O(total memories); its `order_by` fix rides with
the v0.3 storage rework.
