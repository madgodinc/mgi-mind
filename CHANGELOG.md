# Changelog

## 0.8.0 - one cross-platform binary that is itself the MCP server

A single Rust binary now speaks MCP over stdio directly (`mgimind mcp`), replacing the
three-process stack (Node MCP server -> Unix-socket daemon -> per-call CLI). The process
lives for the whole session, so the embedding models load once and stay warm with no
daemon to run. This also removes the only Unix-only code (`UnixListener`), so the
Windows build compiles, and drops the Node/npm dependency entirely. Net change: about
450 fewer lines.

### Added
- **`mgimind mcp`** - hand-rolled JSON-RPC 2.0 MCP server over stdio (no SDK
  dependency). Implements `initialize`, `tools/list` and `tools/call` for all 21 tools,
  plus `ping` and the lifecycle notifications. Tool-execution failures are returned as
  a result with `isError: true` (not a JSON-RPC error), so a failing tool never drops
  the client session. Requests are handled sequentially - one stdio client needs no
  session pool.
- **Automatic Qdrant startup.** `mgimind mcp` brings up the bundled Qdrant (detached,
  in its own process group on Unix / `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on
  Windows) so Qdrant outlives the session and a minimal user never runs `serve` by
  hand. Soft on the "two sessions start at once" race.
- **Antivirus / quarantine diagnosis in `doctor`.** When a download reports success but
  the file is missing afterward, `doctor` now says so ("likely antivirus/SmartScreen
  quarantine") instead of silently looping on `--fix`.
- **MCP round-trip integration test** (`mcp_add_then_search_roundtrip`): drives
  `mgimind mcp` over real stdin/stdout and asserts add -> search retrieves, and that
  every stdout line is valid JSON-RPC.

### Changed
- The 14 tools that previously shelled out to the CLI now call shared text-returning
  `run_*`/`render_*`/`build_*` functions in-process. All download/progress output moved
  from stdout to stderr so the MCP stdout channel stays pure JSON-RPC.
- `vault_list` is now terminal-only (like `vault_get`/`vault_store`): it needs the
  master password on a TTY, which MCP has no access to, so it returns instructions
  instead of failing.
- Logs go to stderr in every mode.

### Removed
- The Unix-socket daemon (`src/daemon.rs`, `mgimind daemon`) and the Node MCP server
  (`mcp-server/`). Their job - keeping models warm and bridging the assistant - is now
  done by the single `mgimind mcp` process.

### Testing
- **The retrieval tests now run on every OS, including Windows (the main target).**
  `setup_model_home` previously symlinked the model (`std::os::unix::fs::symlink`),
  which forced `add_then_search` and the MCP round-trip to be `#[cfg(unix)]` - so the
  add -> search path had no automated coverage on Windows. The helper now copies the
  model dir recursively (portable) and the tests are no longer gated to Unix.
- **`MGIMIND_HOME` env override** for the data dir. `dirs::home_dir()` ignores `$HOME`
  on Windows, so a `$HOME` override could not isolate the data dir there; `MGIMIND_HOME`
  works on all three OSes. Tests use it to isolate; power users can relocate the data
  dir with it. Test `config.json` is now built with `serde_json` so Windows paths
  (`C:\...`) are escaped correctly instead of producing invalid JSON.
- **Windows integration job** in CI. Linux runners use a Qdrant service container;
  Windows runners cannot, so the job starts the binary's own bundled Qdrant (`serve`)
  and runs the same lifecycle + add -> search tests against it.

### Distribution
- Release workflow builds Linux/macOS/Windows binaries on a tag and publishes them to
  GitHub Releases, so users download instead of building.

## 0.7.4 - retrieval test is now a real CI gate

The 0.7.3 `add -> search` integration test only ran locally, because CI did not
provide the embedding model, so it could not catch regressions on its own. CI now
downloads the models once (cached, keyed on `integrity.rs`), runs `doctor --fix`, and
passes `MGIMIND_IT_MODELS` + `ORT_DYLIB_PATH` to the integration job. The full
retrieval path (add -> embed -> hybrid search) is now exercised automatically against
the Qdrant service container, not just the library lifecycle.

## 0.7.3 - MCP fact-invalidate + a real retrieval integration test

Follow-up to the 0.7.2 review.

### Fixed
- **`mind_fact_invalidate` was missing from the MCP server.** Fact supersession was
  documented as "query, invalidate, add" for the agent, but the invalidate step had
  no MCP tool, so over MCP the agent could not actually do it. Added the tool;
  AI_INSTRUCTIONS now lists it as an MCP tool (not CLI-only).

### Tests
- New integration test `add_then_search_finds_the_memory`: creates a library, adds a
  memory, runs a paraphrased hybrid search, and asserts the memory is retrieved. This
  exercises the real path (add -> embed -> hybrid search), not just the library
  lifecycle. Gated on `MGIMIND_IT_MODELS` + `ORT_DYLIB_PATH` so CI without the model
  skips it; verified locally against a real Qdrant and the e5 model.

## 0.7.2 - Code-review fixes (security, correctness, tests)

A line-by-line review found real issues; this release closes the actionable ones.

### Security
- **Supply chain (#6 regression).** The default stack (multilingual-e5-base and
  bge-reranker-base) was downloading with no checksum because only the old MiniLM
  was pinned. Both models' quantized ONNX and tokenizer now have pinned SHA-256 and
  download fail-closed.
- **Vault store over MCP.** `mind_vault_store` no longer accepts the secret value
  over MCP (it would land in process argv, and needs a TTY anyway). It now returns
  terminal instructions, matching `mind_vault_get`.
- **Daemon socket.** The Unix socket is chmod 0600 so another local user cannot read
  or write the whole memory. Per-connection bytes are capped (no OOM from a huge
  line), and a transient accept error no longer kills the daemon.

### Correctness
- **add_memory now chunks (#3).** The main write path (including MCP `mind_add`) no
  longer silently drops everything past 512 tokens; long content is split into
  chunks. `add_memory` returns the number of chunks stored.
- **Vault durability (#4, #11).** `atomic_write` fsyncs the parent directory so the
  rename is durable after a crash. Argon2id parameters are pinned (not
  `Argon2::default()`), so a crate upgrade cannot make existing vaults undecryptable.
- **Context briefing (#10).** The key-facts section is ordered newest-first
  (`order_by created_at`) instead of an arbitrary page; the facts collection gets a
  `created_at` index.
- **Consistent score.** Reranked results map the cross-encoder logit through a
  sigmoid to 0..1, so the `score` field means the same thing with rerank on or off.
- **No double model build.** Embedder and reranker sessions use `get_or_try_init`,
  so concurrent first calls build the ONNX session exactly once.

### Tests
- Unit tests expanded to 28 (daemon request parsing, integrity pins, config
  defaults + legacy-config shape, vault encrypt/decrypt property roundtrip on varied
  payloads, chunking, sparse vectors).
- New black-box integration test (`tests/cli_integration.rs`) that drives the built
  binary against a real Qdrant. CI runs it against a Qdrant service container, and
  the build/clippy/unit matrix now covers Linux, macOS, and Windows.

### Known, not done here
- Fact supersession for single-valued predicates (#13) is still not implemented
  (dedup and soft-delete are); the facts collection stores an unused dense vector;
  embedding is not batched; the daemon serializes inference under one mutex. These
  are tracked, not closed.

## 0.7.1 - Sequence-length fix and resilient migrate

A real-data migration of 12,587 entries surfaced two issues.

### Fixed
- Inputs longer than the model's 512-token limit crashed ONNX inference with
  "invalid expand shape". The embedder and reranker now truncate to 512 tokens.
- `migrate` aborted the whole run on a single failing entry. It now logs and skips
  the entry, continues, and reports how many were skipped.

### Notes
- Reranking is on by default. `bge-reranker-base` is strong on English (the target
  audience) and improves precision there. It does lower Russian ranking, so turn it
  off (`rerank_enabled=false`) or use a stronger multilingual reranker if you need
  good Russian results.

## 0.7.0 - Hybrid search: dense + sparse + RRF (audit #23)

Dense (semantic) retrieval misses exact rare terms; lexical (BM25) retrieval
misses paraphrases. The memories collection now carries **both** and fuses them.

### Changed
- **Named vectors** on the memories collection: `dense` (e5, cosine) + `sparse`
  (BM25-style, with Qdrant's **IDF modifier** applied server-side). `add_memory`
  and `migrate` write both.
- **`search` is hybrid**: a Qdrant Query API call with two prefetches (dense NN +
  sparse NN) fused by **Reciprocal Rank Fusion (RRF)**, then cross-encoder reranked
  (#22). A library filter applies to both arms.
- Sparse vectors are unicode-aware term-frequency (lowercased, split on
  non-alphanumeric - handles Cyrillic; tokens hashed to u32 indices).

### Validated
- Runtime-tested end-to-end: exact rare terms (`fossilize_replay`, `gamemoderun`)
  surface via the lexical arm, while semantic queries ("как стим компилирует
  шейдеры") still hit via dense - fused and reranked correctly.

### Audit complete
- 22 of 27 issues fully fixed, 5 partial (non-blocking polish), 0 deferred.
- **Operational (not audit):** live cutover - deploy v0.7.0, `doctor --fix`
  (fetch e5 + reranker), `migrate` (re-embed at the new dense+sparse 768-dim
  schema) - plus daemon autostart.

## 0.6.0 - Cross-encoder reranker (audit #22)

Dense retrieval is fast but coarse. A cross-encoder now re-orders the top-K by
scoring each (query, passage) pair jointly - a big precision win.

### Added
- **`src/reranker.rs`**: `bge-reranker-base` (XLM-R, multilingual incl. RU;
  quantized ONNX, ~279 MB, CPU-ok). All candidate pairs run in a single padded
  batch (one ONNX pass). `search` fetches `rerank_top_k` (default 20) candidates by
  dense similarity, reranks, and returns `limit`. Reranking scores the **full**
  content; tier truncation is display-only, applied after ordering.
- Config: `rerank_enabled` (default true), `rerank_model` (`bge-reranker-base`),
  `rerank_top_k` (20). `doctor --fix` fetches the reranker model.
- **Best-effort**: any reranker failure (missing model, inference error) leaves the
  dense order untouched - reranking is a quality boost, never a hard dependency.

### Validated
- Runtime-tested: for «почему в доте мало фпс хотя видеокарта мощная» the reranker
  sharply separated relevance (1.07 / 0.83 / −0.66) where dense was a flat
  0.86 / 0.84 / 0.82.

### Still open
- #23 hybrid/BM25 search (e5 is dense-only → needs a separate sparse path).
  Operational: daemon autostart + live cutover (re-embed at 768-dim + reranker).

## 0.5.0 - Multilingual embedder support: e5-base (audit #21)

The English-only MiniLM is replaced as the default by **multilingual-e5-base** -
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

## 0.4.0 - Single-collection storage (audit #18)

Memories moved from one Qdrant collection per library (`mem_<library>`) to a single
`memories` collection with a `library` payload field. This is a storage-layout change
- run `mgimind migrate` once to import existing data.

### Changed
- **One `memories` collection** with payload indexes on `library` (keyword) and
  `created_at` (datetime). Search runs a **single query** - true global top-k, or a
  `library`-filtered query - instead of scanning N collections and merging.
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
  hybrid/BM25 search (#23) - each needs a new model + full re-embed.

## 0.3.0 - Daemon (audit #16)

The MCP server spawned a fresh `mgimind` process per call, reloading the ONNX
session + tokenizer every time. This release adds a long-lived daemon so the model
stays warm.

### Added
- **`mgimind daemon`** (`src/daemon.rs`): loads the embedding model once and serves
  newline-delimited JSON requests over a Unix socket (`~/mgimind/daemon.sock`).
  Supported: search, add, context, history, fact_add, fact_query, stats, ping.
- **Thin MCP client**: `mcp-server/index.js` routes embed-heavy/common tools to the
  daemon and **falls back to spawning the CLI** when the socket isn't there - the
  daemon is a pure optimization, never a hard dependency.
- Shared render helpers (`cli::render_search/render_history/render_facts/build_stats/
  build_context`) so daemon and CLI output are identical (one source of truth).

### Validated
- End-to-end against live data (12 587 memories read correctly via the daemon).
- Latency: warm daemon add ~31ms vs cold CLI add ~175ms (~5.6×). The audit's "2-5s"
  figure is the cold-disk/first-load case; the model is normally OS page-cached.

### Still open
- **Operational:** autostart entry for the daemon + cutover of the live instance.
- Deferred audit items unchanged: single-collection (#18), code embedder / reranker
  / hybrid search (#21-23); `history` O(total) rides with #18.

## 0.2.1 - Post-review fixes

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
  code embedder / reranker / hybrid search (#21-23).

## 0.2.0 - Audit hardening

This release rebuilds the data and security layers around the findings of a full
code audit. See [`AUDIT_STATUS.md`](AUDIT_STATUS.md) for the complete issue-by-issue
accounting. It is **API-compatible** with 0.1.x data on disk (config gains
defaulted fields; existing Qdrant collections keep working).

### Security & data integrity
- **Atomic writes** for config, vault, salt, sessions and exports (temp + fsync + rename) - a crash can no longer corrupt these files.
- **Vault** master password is now read **without echo** (`rpassword`) and zeroized after key derivation; reads no longer rewrite the encrypted blob; the plaintext `vault.count` file is gone.
- **Vault over MCP**: secrets are never returned through the MCP/LLM channel and the master password is never blank - `mind_vault_get` directs you to a terminal.
- **Download integrity**: artifacts are fetched over HTTPS (native `reqwest`, no `curl`) and verified against pinned SHA-256 hashes (linux-x64 ONNX Runtime, Qdrant, default model); unknown targets warn instead of trusting blindly.
- **Qdrant** is bound to `127.0.0.1` and supports an optional API key.

### Correctness
- **Deterministic content-addressed IDs** (`UUIDv5` of library + content): re-adding identical content is an idempotent upsert - no duplicates, no read-before-write race. Same for facts (`subject,predicate,object`).
- **`history`** is sorted newest-first; **`export`** paginates fully (no silent 10k cap).
- **Embedding dimension** is configurable and validated on every operation (model-swap safety).
- **Knowledge graph**: queries match subject/predicate/object via a full scan + filter (nothing lost outside a top-K window); `invalidate` is a soft delete (`valid=false`) that queries honor.
- **Sessions** are per-agent: no shared `.current` to clobber, second-precision + random filename suffixes, `--agent`-scoped `end`/`last`.

### Performance & portability
- Tokenizer loaded once and cached (no per-embed disk read).
- Native gzip/tar/zip extraction and native gzip+tar backup/restore - no `tar`/`unzip` shellouts.
- Improved chunking: overlap between chunks and hard-split of overlong lines.
- Tier truncation breaks on word boundaries.

### Tooling
- First unit-test suite (`cargo test`) and GitHub Actions CI (fmt, clippy `-D warnings`, test, `cargo audit`).

### Deferred to 0.3 (see AUDIT_STATUS.md)
- Long-lived daemon + thin MCP client (kills per-call model reload).
- Single-collection storage with `library` payload filter (parallel/global ranking).
- Code-capable embedder + cross-encoder reranker + hybrid (BM25/RRF) search - these change the vector dimension and require a re-index, done at deploy time.

## 0.1.0
- Initial release.
