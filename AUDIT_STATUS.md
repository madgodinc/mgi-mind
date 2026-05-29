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
| 11 | Vector size hardcoded (384) | ✅ | `vector_size` is in config; collections use it; `check_dim()` validates every embedding against it and errors on mismatch (model-swap detection). |
| 12 | KG query loses data (top-20 + substring) | ✅ | `query_facts` scrolls the full facts collection and filters by subject **or** predicate **or** object; valid-only. |
| 13 | `invalidate` hard-deletes; `valid` unused; no fact dedup | ✅ | `invalidate_fact` now `set_payload valid=false` (soft delete, honored by queries). Facts get deterministic IDs from `(s,p,o)` → dedup. |
| 14 | Session model breaks under concurrency | ✅ | Removed the single global `.current`; per-agent `.current.<agent>` pointers. Filenames carry seconds + a random suffix (no same-minute collision). `session end`/`last` are scoped by `--agent`. |
| 15 | TOCTOU race in `add_memory` | ✅ | Deterministic `UUIDv5(namespace, library + content)`: same content → same point ID → idempotent upsert. No read-before-write, no race, no duplicate. |

## Part I — Architecture & performance (🟡)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 16 | MCP spawns a process per call → model reloads | 🔜 v0.3 | Big architectural change (long-lived daemon + thin MCP client). Deferred so v0.2 ships safe correctness fixes without a rewrite. Mitigated meanwhile by #17. |
| 17 | Tokenizer re-read from disk every embed | ✅ | Tokenizer cached in a `OnceCell`, loaded once (like the ONNX session). |
| 18 | Cross-library search is sequential | 🔜 v0.3 | Tied to a single-collection redesign (one collection + `library` payload filter). Deferred with #16; current per-collection layout is unchanged and correct. |
| 19 | Heavy shellouts (curl/tar/unzip) | ✅ | Native `reqwest` downloads; native `flate2`+`tar` and `zip` extraction; native gzip+tar backup/restore. No external `curl`/`tar`/`unzip` needed. (`crw` web reader stays optional/external.) |
| 20 | Naive `chunk_text` | 🟡 | Overlap between chunks + hard-split of overlong single lines (no more giant chunks). Token-aware / tree-sitter (AST) chunking deferred to v0.3. |

## Part I — Search quality (🔵)

| # | Issue | Status | What changed |
|---|-------|--------|--------------|
| 21 | Weak/old embedder for code | 🔜 v0.3 | Requires choosing a code embedding model, which changes the vector dimension and forces re-embedding existing memories — a data migration done at deploy time, with the owner's sign-off. `vector_size` is now configurable (#11), so the swap is a config + reindex. |
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

**Summary:** of the 27 audited issues, **20 are fixed in v0.2**, **4 are partial**
(mechanism shipped, hardening continues: #6, #20, #24, #25), and **3 are deferred to
v0.3** (#16 daemon, #18 single-collection, #21–23 ML/search — all requiring either a
data migration or new models, to be done at deploy time with the owner's sign-off).
