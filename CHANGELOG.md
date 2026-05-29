# Changelog

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
