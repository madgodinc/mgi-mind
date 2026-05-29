# Changelog

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
