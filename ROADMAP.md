# Roadmap

This file is the **public, committed scope** for upcoming releases. It is
honest about what is decided vs. what is still candidate, about what shipped
vs. what was cut, and about what each version is being measured against.
Numbered versions name a release; "horizon" names a set of directions that
are not yet committed to a specific tag.

Discipline:

- **Bench-on-the-wall before features.** Each retrieval-affecting release
  publishes a ΔR@k on `BENCHMARKS.md`. A regression on the headline
  configuration blocks the release.
- **The default install path is the headline.** Numbers from an opt-in
  GPU / FP16 / paid-API ablation are reported, but never put in the
  release title.
- **Critic-checked.** Anything labeled "candidate" has not yet survived a
  full critic round; the v3.0 block below is explicitly a candidate set,
  not a promise.
- **History is not rewritten.** Shipped blocks are edited to say "shipped
  in vX.Y"; cut items are marked cut with the reason. Stale promises are
  removed, not left to rot.

## Shipped — the v1.x line (v1.0 … v1.7, 2026-06)

The semver-stable line. **R@5 = 98.2% on LongMemEval-S** on the default
install path (all-MiniLM-L6-v2 INT8 + reranker, CPU); the GPU ablation
(e5-base FP16 + reranker) reads 99.2%. What the v1.x line froze:

- **v1.0** — the frozen contracts: asymmetric `Qdrant now → md says`
  reconcile diff, the `MGIMIND_MODEL_VARIANT={cpu|gpu|auto}` switch, the
  MCP surface, procedural memory (`mind_learn` / `mind_recall` /
  `mind_procedure_outcome`) with the 227-pair Д6 dataset, quarantine +
  relevance gate + best-effort active retrieval, audit log, ephemeral
  viewer, secret scrub, vault, session liveness. Breaking any of these
  required the v2.0 bump.
- **v1.1** — tool-surface consolidation. 15 single-verb tools replaced by
  5 action-dispatched verbs (`mind_quarantine` / `mind_vault` /
  `mind_session` / `mind_fact` / `mind_library`). The 15 singletons stayed
  live as deprecated aliases, each flagged `"deprecated": true` with a
  description promising removal at v2.0. **(That removal slipped past v2.0
  and is being executed at v2.2 — see the v2.x section.)**
- **v1.2** — encrypted backup, **re-scoped**. The originally-planned S3
  `backup push`/`pull`, restic-style chunking, and `backup verify`
  monotonicity were **never built and are cut**. What shipped is a local
  `mgimind backup [--encrypt]` archive (AES-256-GCM, Argon2id), reusing
  the `vault.rs` primitives. The security intent survives in two open v2.x
  gate items: the ciphertext-only assertion on the backup write path, and
  the key-loss threat model.
- **v1.3** — REST + portable format, **split by fate**. Shipped: the
  optional HTTP transport as `mgimind serve-http` with bearer tokens
  (landed in the v1.7 window, matured through v2.0). **Cut:** OAuth 2.1 /
  PKCE / Dynamic Client
  Registration (multi-user remote governance is a v3 Candidate C concern,
  not v2.x) and the markdown live mirror (the md escape hatch already
  covers the crisis-edit case; a live mirror flirts with the anti-roadmap
  "md as source of truth" line). **Deferred:** the `memory.json` portable
  format → v2.4 candidate; the token-window / AST-aware chunking upgrade →
  the bench-gated backlog. **Superseded:** procedure auto-confirm, by the
  typed `mind_outcome` signal in v1.5.
- **v1.4** — bi-temporal facts + supersession. Each fact carries a
  `valid_until` alongside its `created_at`, so its active interval is
  `[created_at, valid_until)`; a new fact on the same `(subject, predicate)`
  axis auto-sets the prior fact's `valid_until` instead of producing a
  silent contradiction; `mgimind doctor` surfaces `(s, p)` pairs with more
  than one active object. No automatic deletion. (The `as_of` reader on
  `mind_fact_query` that actually consults these timestamps landed later,
  in the v1.7 window — PR #57; v1.4 only wrote the interval.)
- **v1.5** — install-mode profiles, typed `mind_outcome` outcome signals,
  and the active re-test pass. **Honest delta:** the decay / consolidation
  work motivated in the comparison docs — time-weighted coldness in browse
  and `consolidate --archive-cold` — actually landed later, in the v1.7
  window (PR #56 / #59); `consolidate --auto` as a default path did **not**
  ship, and cron remains the setup hint. Decay is a ranking/archival
  signal, never a delete signal.
- **v1.6 / v1.7** — hardening and CLI surfacing (batched payload reads,
  `mind_outcome` CLI, facts/stats inspection, cardinality bulk-apply,
  Windows stack-overflow fix, the bi-temporal `as_of` fact reader, and
  time-weighted coldness in browse + `consolidate --archive-cold`), then
  `/kv` blob store, the `/audit` Track-3 trail, and `mgimind reindex`
  (rebuild the index after an embedding-model switch). v1.7.0 is the last
  v1.x tag.

## Shipped — v2.0.0 (2026-07-04)

v2.0 is a **gate**, not a feature dump: "ready for arbitrary external
users" once several agents read and write one brain at once. What closed
in v2.0.0:

- **Per-token library ACL.** `--agent-token NAME:TOKEN:lib1,lib2` scopes a
  token to a library allowlist, enforced **fail-closed**: a scoped token
  reaches only `/memory/{search,browse,recall,add,ingest}` + `/health` +
  `/should-search`; every other route is 403. Writes must name an
  allowlisted library; disallowed reads are 403; unspecified reads get the
  allowlist injected as their filter.
- **Per-author write flood control.** `serve-http` caps writes per author
  over a rolling 60s window (`write_quota_per_min`, default 600); a looping
  agent hitting `/memory/add` (which skips the ingest gate) now gets 429
  instead of flooding the pool.
- **Duel verdict on `/fact/add`** (`recorded` / `won` / `contested` /
  `quarantined`) + a `/fact/contested` route, so a losing agent no longer
  reads a bare id as "stored as truth".
- **Embedding-model stamp + fail-closed startup.** Each point records its
  `embed_model`; `serve-http` refuses to start on a mismatch, turning a
  silent dimension-preserving model swap into a clear "run `mgimind
  reindex`".
- **BREAKING (serve-http):** `/memory/search` and `/memory/recall` return
  structured JSON by default instead of the `{ok, result:"<text>"}`
  envelope.

## Shipped — v2.1.0 (2026-07-04)

Documentation and recipes on top of the 2.0 surfaces — no server change.
Two multi-agent orchestrators (Hermes, OpenClaude) can now use mgi-mind as
their shared memory; per-agent tokens keep a shared pool attributable.

## Shipped — v2.1.1 (2026-07-04)

Docs truth-sync (version markers to 2.1.1 across README en/ru/zh, SECURITY,
AI_INSTRUCTIONS; this roadmap actualization; cruft prune) plus ACL test
hardening: the v2.0 library-ACL decision extracted into pure cores
(`scope_libs`, `scoped_route_allowed`, `parse_agent_tokens`) with an
adversarial suite — the Д7 "fuzz-testing the enforcement" gate item. No
behavior change.

## Shipped — v2.2.0 (2026-07-05) — local feature parity, batch 1

Two capabilities stolen from the 2026 memory-layer field and rebuilt local
and LLM-free (additive; no behavior change):

- **Pinned memory blocks (Letta-style core memory).** `mgimind block
  set|get|list|rm` + MCP `mind_block` manage a few small named notes
  (persona / user / project) in `blocks.json`, injected at the top of every
  context render — always-true context, not ranked retrieval — with 4 KB /
  32-block caps.
- **Procedures → instructions export (LangMem-style).** `mgimind export
  --format instructions` renders verified error→fix procedures as a portable,
  agent-ready markdown block; deterministic and LLM-free (outcomes are
  already typed-verified).

## Shipped — v2.3.0 (2026-07-05) — local feature parity, batch 2

- **Multi-hop fact-graph traversal (Graphiti / Memary-style).** `mgimind graph
  <entity> --hops N` — a bounded, cycle-safe BFS over the facts already in
  Qdrant, rendered as an indented tree of directed edges. No extra store.
- **Local profile snapshot (Supermemory-style).** `mgimind export --format
  profile` — a compact, prompt-ready snapshot (pinned blocks + current facts +
  verified procedures), computed locally with no LLM summarization.

Remaining steal-list items (min-confidence filter, memory TTL, local
Memory-Router proxy) land as later v2.x minors; retrieval-touching ones carry a
ΔR@k bench. Anything needing an LLM call stays on the v3 candidate ledger.

## v2.x — closing the gate

The remaining v2.0-gate items and near-term minors. None of these touch the
headline retrieval path, so unless a line says "bench" it ships without a new
ΔR@k.

- **Deferred — remove the 15 deprecated MCP aliases.** Their own descriptions
  have promised "Removed in v2.0" since v1.1; v2.0–v2.3 shipped without doing
  it. This is a breaking change that also disrupts existing local tooling, so
  it is **held pending an explicit go/no-go** and takes the next free minor
  when scheduled.
- **✅ Shipped — v2.5.0 (2026-07-19) — macOS install path fixed end to end.**
  An audit of every step from `install.sh` to a warm embedder, prompted by an
  install that failed on someone else's MacBook. Intel Macs are back in the
  release matrix (cross-compiled from the arm64 runner) and pin ONNX Runtime
  1.23.0, the last release shipping `osx-x86_64`; the binary now asks for C
  API 23, which both runtimes serve. Two latent extractor bugs on the Intel
  path fixed (leading `./` in tar members, `libonnxruntime.dylib` being a
  symlink there). The installer writes `~/.local/bin` into the shell profile,
  since zsh does not carry it and the old warning scrolled away under the
  model download. Still open: releases are ad-hoc signed, not notarized, so a
  browser-downloaded binary needs a Gatekeeper override.
- **✅ Shipped — v2.4.0 (2026-07-05) — multi-tenant confinement gate closed
  (Д7) + tamper-evident audit.** The full confinement set landed; the ACL
  enforcement is now confined, not just unit-covered:
  - Scoped-token `/memory/ingest` confines fact/procedure candidates
    (skip-with-counter) — they would otherwise land in the GLOBAL stores,
    escaping the library allowlist. **Closed a real ACL bypass**; the
    "fail-closed per-token ACL" claim is now true.
  - `/memory/by-agent` confined to the token's allowlist (server-injected
    library filter) instead of a blanket 403 — confinement, not lockout;
    falsifiable contract test.
  - Ciphertext-only newtype on the local backup write path — the sole writer
    accepts only sealed `Ciphertext`.
  - Viewer `--libraries` confinement — memory views filtered to the allowlist;
    endpoints that span all libraries or the global fact graph (audit, graph,
    pulse, ingest feed, consolidate preview, node-by-id) and all mutations are
    fail-closed (403) when confined.
  - Tamper-evidence **audit-log hash-chain** + `mgimind audit verify` (BLAKE3
    prev-hash chained through the single-writer path; SECURITY.md promise →
    shipped).
- **v2.5 — portability + Memory-Router (candidate).** The `memory.json`
  portable export/import format (v1.3 leftover); the local Memory-Router
  OpenAI-compatible proxy (biggest adoption lever, no ranking change).
- **v2.5+ — encrypted collection-at-rest.** Qdrant reads its storage dir
  in plaintext, so real at-rest encryption is its own minor. Ship the
  **threat model first** (key compromise, key loss, rollback; "at-rest =
  encrypted backup + OS full-disk-encryption guidance"), implementation
  after.
- **Bench-gated backlog (dedicated bench night, not a calendar minor).**
  Self-editing memory (Д5) — ships only if it does **not** regress R@k on
  LongMemEval-S and does not raise the v1.4 contradiction rate. Token-
  window / AST-aware chunking (audit #20) — retrieval-path, **mandatory
  blocking ΔR@k**. Neither ships without a full headline-config bench.
- **External security review** of the (re-scoped, local) backup module
  stays listed as an open pre-launch item, with that module as its input.

New local-first, no-LLM capability features stolen from the 2026 field
(pinned memory blocks, an auto-derived profile object, memory TTL, a
min-confidence retrieval filter, procedures→instructions export, multi-hop
fact traversal, heuristic temporal query operators, a local Memory-Router
proxy) are slotted as their own v2.x minors after the gate closes; each
retrieval-touching one carries a bench. Anything that fundamentally needs
an LLM call lands on the v3 candidate ledger below, not here.

## v3.0 horizon — candidate set, not a promise

v3.0 is far enough that committing to a single shape would be dishonest.
These are **non-exclusive directions**, each gated on signal from real
users of v1.0–v2.x. Whichever crosses both a critic-checked spec and a
measurable user pull gets the v3.0 tag; the others stay on this list or
fall off it.

Candidate A — **Local LLM extractor for the write-gate.** A small local
model (the `extractor.rs` Qwen2.5-GGUF scaffold, off by default) deciding
"should this be a memory" / routing ADD-UPDATE-DELETE-NOOP over existing
similar memories — the shape Mem0 and LangMem build their whole write path
on. Stays local (GGUF), stays opt-in. Risk: a second runtime model.

Candidate B — **Judge-eval QA mode.** An opt-in, explicitly paid-API path
for like-for-like QA comparison with Mem0 / Zep / LangMem. Numbers
reported separately from the zero-API R@k headline and tagged "+LLM cost".
Answers "but those systems report QA accuracy" without contaminating the
core metric.

Candidate C — **Cross-agent / cross-machine governance.** The v2.0 library
ACL is the first brick; the v3-shaped scope is the governance layer
(per-agent scopes, a `mind_grant` primitive, OAuth/DCR remote) that turns
mgi-mind into a multi-user company brain.

Candidate D — **Schema packs / pluggable taxonomy.** Teach the system new
typed entities (Person, Company, Deal) at runtime, à la GBrain schema
packs / cognee ontologies. Lands only with a clear migration story.

Candidate E — **Self-wiring graph between memories.** A non-LLM auto-link
mechanism over memories (not just facts). Research note from the 2026
competitive sweep: **GLiNER-class zero-shot NER runs in ONNX**, so entity
extraction + shared-entity auto-linking could ride the existing ONNX
runtime with no LLM — cheaper than this candidate originally assumed. Still
unproven on R@k and squarely on the hot path → a bench-first spike, still
v3.

## Anti-roadmap (things we are explicitly not doing)

- Obsidian plugin / live Obsidian sync. The three Obsidian-shaped requests
  are covered separately ("see" → viewer, "edit in a crisis" → md escape
  hatch, "sync between devices" → encrypted backup).
- Markdown as a live source of truth. Qdrant is canonical; md is export +
  reconcile.
- OAuth / DCR multi-user remote is **not** a v2.x item — it is v3
  Candidate C.
- A 50+ MCP-tool sprawl. New tools come from a real gap, not from "we
  could add this".
- Cloud-hosted mode where mgi-mind sees user data. The local-first
  contract is non-negotiable through at least v3.0.
- Sales / marketing pumps. Discoverability comes from measurable artifacts
  (releases, benchmarks, an eventual preprint), not outreach.

## How this file gets updated

A release that ships a scoped feature edits its block to say "shipped in
vX.Y" with the tag. Cut items are marked cut with the reason. v3.0
candidates either earn promotion to a numbered version (with a
critic-checked spec) or fall off the list. The file should never grow
stale promises.
