# Changelog

## 0.6.0 — Cross-encoder reranker (audit #22)

Dense retrieval is fast but coarse. A cross-encoder now re-orders the top-K by
scoring each (query, passage) pair jointly — a big precision win.

### Added
- **`src/reranker.rs`**: `bge-reranker-base` (XLM-R, multilingual incl. RU;
  quantized ONNX, ~279 MB, CPU-ok). All candidate pairs run in a single padded
  batch (one ONNX pass). `search` fetches `rerank_top_k` (default 20) candidates by
  dense similarity, reranks, and returns `limit`. Reranking scores the **full**
  content; tier truncation is display-only, applied after ordering.
- Config: `rerank_enabled` (default true), `rerank_model` (`bge-reranker-base`),
  `rerank_top_k` (20). `doctor --fix` fetches the reranker model.
- **Best-effort**: any reranker failure (missing model, inference error) leaves the
  dense order untouched — reranking is a quality boost, never a hard dependency.

### Validated
- Runtime-tested: for «почему в доте мало фпс хотя видеокарта мощная» the reranker
  sharply separated relevance (1.07 / 0.83 / −0.66) where dense was a flat
  0.86 / 0.84 / 0.82.

### Still open
- #23 hybrid/BM25 search (e5 is dense-only → needs a separate sparse path).
  Operational: daemon autostart + live cutover (re-embed at 768-dim + reranker).

## 0.5.0 — Multilingual embedder support: e5-base (audit #21)

The English-only MiniLM is replaced as the default by **multilingual-e5-base** —
a big retrieval-quality win for Russian/mixed content, practical on CPU (768-dim,
~278M, runs quantized). The embedder is now model-architecture-flexible.

### Changed
- **Default model → `multilingual-e5-base`** (768-dim). Existing MiniLM configs keep
  working unchanged (serde preserves their `model_name`/`vector_size`/pooling).
- **Embedder is architecture-flexible** (`pooling` = mean|cls; optional
  `token_type_ids`): supports both BERT-family (MiniLM) and XLM-R (e5) models.
  Pooling math is unit-tested.
- **Query/passage prefixes** (`query_prefix`/`passage_prefix`): e5 requires
  "query: " / "passage: ". `search` embeds as a query, stored memories/facts as
  passages. MiniLM uses empty prefixes (no behaviour change).
- **Model download is source-aware**: e5 ONNX is fetched (quantized) from the Xenova
  mirror; sentence-transformers models keep their path.

### Validated
- e5-base runtime-tested on an isolated instance with the real quantized model: RU
  queries returned the correct top result every time (e.g. «искусственный интеллект
  для трансляций» → Aurora 0.79; «что приготовить на обед» → борщ 0.82, not the tech
  entries). Confirms the e5 ONNX path: no token_type_ids, mean pooling, query/passage
  prefixes, 768-dim.

### Deploy step (not automatic)
- Live cutover: `mgimind doctor --fix` fetches the e5 model, then `mgimind migrate`
  re-embeds existing memories at 768-dim under e5.

### Still open
- #22 cross-encoder reranker, #23 hybrid/BM25 search (e5 is dense-only → needs a
  separate sparse path). Operational: daemon autostart + live cutover.

## 0.4.0 — Single-collection storage (audit #18)

Memories moved from one Qdrant collection per library (`mem_<library>`) to a single
`memories` collection with a `library` payload field. This is a storage-layout change
— run `mgimind migrate` once to import existing data.

### Changed
- **One `memories` collection** with payload indexes on `library` (keyword) and
  `created_at` (datetime). Search runs a **single query** — true global top-k, or a
  `library`-filtered query — instead of scanning N collections and merging.
- **`history` is no longer O(total)**: it uses Qdrant `order_by` over the
  `created_at` datetime index to return the newest N directly (fixes the post-0.2
  review finding). 
- Libraries are tracked in a small `libraries.json` registry; counts always come
  from live data (`count` + filter), never the file.

### Added
- **`mgimind migrate [--purge]`**: imports legacy `mem_*` collections into
  `memories`. Re-embeds from stored content (no raw-vector extraction), preserves
  each entry's original `created_at`, idempotent (deterministic IDs), and with
  `--purge` deletes the old collections after a successful copy.

### Validated
- Isolated-instance runtime test: global + library-filtered search, ordered
  `history`, per-library `stats`, `drop` (delete-by-filter), and `migrate` (with
  `created_at` preserved and content re-embedded) all verified end-to-end.

### Still open
- **Operational:** daemon autostart + cutover of the live instance (now also: run
  `migrate` on the live data during cutover).
- Deferred audit items: code embedder (#21), cross-encoder reranker (#22),
  hybrid/BM25 search (#23) — each needs a new model + full re-embed.

## 0.3.0 — Daemon (audit #16)

The MCP server spawned a fresh `mgimind` process per call, reloading the ONNX
session + tokenizer every time. This release adds a long-lived daemon so the model
stays warm.

### Added
- **`mgimind daemon`** (`src/daemon.rs`): loads the embedding model once and serves
  newline-delimited JSON requests over a Unix socket (`~/mgimind/daemon.sock`).
  Supported: search, add, context, history, fact_add, fact_query, stats, ping.
- **Thin MCP client**: `mcp-server/index.js` routes embed-heavy/common tools to the
  daemon and **falls back to spawning the CLI** when the socket isn't there — the
  daemon is a pure optimization, never a hard dependency.
- Shared render helpers (`cli::render_search/render_history/render_facts/build_stats/
  build_context`) so daemon and CLI output are identical (one source of truth).

### Validated
- End-to-end against live data (12 587 memories read correctly via the daemon).
- Latency: warm daemon add ~31ms vs cold CLI add ~175ms (~5.6×). The audit's "2–5s"
  figure is the cold-disk/first-load case; the model is normally OS page-cached.

### Still open
- **Operational:** autostart entry for the daemon + cutover of the live instance.
- Deferred audit items unchanged: single-collection (#18), code embedder / reranker
  / hybrid search (#21–23); `history` O(total) rides with #18.

## 0.2.1 — Post-review fixes

A follow-up code review of 0.2.0 found four issues the hardening pass either
over-claimed or introduced. This release closes the tractable ones; the rest are
documented honestly (see [`AUDIT_STATUS.md`](AUDIT_STATUS.md)).

### Fixed
- **Session pointer collision (regression of #14).** `sanitize` mapped every
  non-`[A-Za-z0-9-]` byte to `_`, so `team a`, `team_a`, `team/a`, `team.a` all
  shared one `.current.<agent>` pointer and clobbered each other's session. It is
  now an injective `_HH` escape (the escape byte `_` is itself escaped).
- **`created_at` was reset on re-add.** Content-addressed upserts overwrote the
  whole payload, so re-adding identical content set `created_at = now` and the
  entry jumped to the top of chronological history. The original `created_at` is
  now preserved (read-before-write by id); a separate `updated_at` records the
  re-touch. Applies to both memories and facts.
- **Facts had no dimension guard (#11 gap).** `add_fact` now runs the same
  `check_dim` model-swap check as `add_memory`.
- **Config↔collection dimension mismatch (#11).** `mgimind serve` now checks every
  collection's on-disk vector dimension against the configured `vector_size` and
  warns up front (best-effort; never blocks serve), instead of surfacing a raw
  Qdrant error on the first upsert after a model change.

### Known, still open (documented, not silently dropped)
- **`history` is O(total memories).** Correct (newest-first), but scrolls every
  collection fully. Fine at current scale; the `order_by`-over-datetime-index fix
  rides with the v0.3 storage rework (#16/#18).
- Deferred 0.3 items unchanged: daemon (#16), single-collection (#18),
  code embedder / reranker / hybrid search (#21–23).

## 0.2.0 — Audit hardening

This release rebuilds the data and security layers around the findings of a full
code audit. See [`AUDIT_STATUS.md`](AUDIT_STATUS.md) for the complete issue-by-issue
accounting. It is **API-compatible** with 0.1.x data on disk (config gains
defaulted fields; existing Qdrant collections keep working).

### Security & data integrity
- **Atomic writes** for config, vault, salt, sessions and exports (temp + fsync + rename) — a crash can no longer corrupt these files.
- **Vault** master password is now read **without echo** (`rpassword`) and zeroized after key derivation; reads no longer rewrite the encrypted blob; the plaintext `vault.count` file is gone.
- **Vault over MCP**: secrets are never returned through the MCP/LLM channel and the master password is never blank — `mind_vault_get` directs you to a terminal.
- **Download integrity**: artifacts are fetched over HTTPS (native `reqwest`, no `curl`) and verified against pinned SHA-256 hashes (linux-x64 ONNX Runtime, Qdrant, default model); unknown targets warn instead of trusting blindly.
- **Qdrant** is bound to `127.0.0.1` and supports an optional API key.

### Correctness
- **Deterministic content-addressed IDs** (`UUIDv5` of library + content): re-adding identical content is an idempotent upsert — no duplicates, no read-before-write race. Same for facts (`subject,predicate,object`).
- **`history`** is sorted newest-first; **`export`** paginates fully (no silent 10k cap).
- **Embedding dimension** is configurable and validated on every operation (model-swap safety).
- **Knowledge graph**: queries match subject/predicate/object via a full scan + filter (nothing lost outside a top-K window); `invalidate` is a soft delete (`valid=false`) that queries honor.
- **Sessions** are per-agent: no shared `.current` to clobber, second-precision + random filename suffixes, `--agent`-scoped `end`/`last`.

### Performance & portability
- Tokenizer loaded once and cached (no per-embed disk read).
- Native gzip/tar/zip extraction and native gzip+tar backup/restore — no `tar`/`unzip` shellouts.
- Improved chunking: overlap between chunks and hard-split of overlong lines.
- Tier truncation breaks on word boundaries.

### Tooling
- First unit-test suite (`cargo test`) and GitHub Actions CI (fmt, clippy `-D warnings`, test, `cargo audit`).

### Deferred to 0.3 (see AUDIT_STATUS.md)
- Long-lived daemon + thin MCP client (kills per-call model reload).
- Single-collection storage with `library` payload filter (parallel/global ranking).
- Code-capable embedder + cross-encoder reranker + hybrid (BM25/RRF) search — these change the vector dimension and require a re-index, done at deploy time.

## 0.1.0
- Initial release.
