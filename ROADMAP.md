# Roadmap

This file is the **public, committed scope** for upcoming releases. It is
honest about what is decided vs. what is still candidate, and about what
each version is being measured against. Numbered versions name a release;
"horizon" names a set of directions that are not yet committed to a
specific tag.

Discipline carried over from the prior internal roadmap:

- **Bench-on-the-wall before features.** Each minor release publishes a
  ΔR@k on `BENCHMARKS.md`. A regression on the headline configuration
  blocks the release.
- **The default install path is the headline.** Numbers from an opt-in
  GPU / FP16 / paid-API ablation are reported, but never put in the
  release title.
- **Critic-checked.** Anything labeled "candidate" has not yet survived a
  full critic round; the v3.0 block below is explicitly a candidate set,
  not a promise.

## Where we are — v1.0 (shipped 2026-06-03)

The semver-stable line. **R@5 = 98.2% on LongMemEval-S** on the default
install path (all-MiniLM-L6-v2 INT8 + reranker, CPU). What v1.0 froze:

- Asymmetric `Qdrant now → md says` diff in `mgimind import md`.
- `MGIMIND_MODEL_VARIANT={cpu|gpu|auto}` switch + the FP16 GPU recipe.
- MCP surface (`mind_*`) — 30 tools in v1.0, consolidated to 20
  user-facing tools in v1.1 (singletons kept as deprecated aliases
  until v2.0).
- Procedural memory (`mind_learn` / `mind_recall` /
  `mind_procedure_outcome`) with the 227-pair Д6 dataset.
- Quarantine + relevance gate + best-effort active-retrieval policy.
- Audit log, ephemeral viewer, secret scrub, vault, session liveness.

Breaking any of the above requires v2.0.

## v1.1 — tool surface consolidation (alias phase) — shipped

Replaces 13 single-verb tools with 5 action-dispatched verbs that match
the shape competitors converged on (one tool per object, action selects
the operation). The 13 singletons stay live as deprecated aliases for
the entire v1.x line and are **removed in v2.0**. No breaking change.

- `mind_quarantine(action="list"|"show"|"promote")` — replaces
  `mind_quarantine_list` / `_show` / `_promote`.
- `mind_vault(action="store"|"get"|"list")` — replaces `mind_vault_store`
  / `_get` / `_list`.
- `mind_session(action="start"|"last"|"end")` — replaces
  `mind_session_start` / `_last` / `_end`.
- `mind_fact(action="add"|"query"|"invalidate")` — replaces
  `mind_fact_add` / `_query` / `_invalidate`.
- `mind_library(action="create"|"list"|"delete")` — replaces
  `mind_create` / `mind_list` / `mind_delete`.

All 15 deprecated entries carry `"deprecated": true` in the JSON schema
and a description that starts `DEPRECATED — use mind_X(action="Y"). Removed
in v2.0.`. Well-behaved MCP clients hide deprecated tools; old clients
keep working unchanged.

Live surface for the v1.1 user-facing shape: **20 tools**. Counting
deprecated aliases the `tools/list` returns 35.

`mind_history` is kept as its own tool — "newest N by time" is a
different verb from "find relevant by query" and merging them would
hurt clarity more than it would help surface size. `mind_doctor` and
`mind_stats` are kept separate for the same reason (broken-state vs
counts).

## v1.2 — opt-in encrypted backup

The first of the three Obsidian-shaped user requests: **"sync between
devices"**. Restore-on-another-device is sync. Moved here from v1.1 so
v1.1 could focus on the surface consolidation.

- `mgimind backup push` / `pull` to an S3-compatible bucket, opt-in
  per-install.
- Encryption with the same primitives already in `vault.rs`
  (AES-256-GCM, Argon2id key derivation). No new crypto protocol.
- restic / borg pattern: fixed-size encrypted chunks + encrypted
  manifest. **Threat model documented** (key compromise, rollback/replay,
  cardinality leak via chunk count).
- `mgimind backup verify` checks monotonicity (catches replay), not just
  per-chunk integrity.

**Out of scope for v1.2.** Live multi-device sync (last-writer-wins is the
wrong default for a memory store); REST server; cloud-hosted mode.

## v1.3 — REST + portable format + reconcile polish

Driven by the second Obsidian-shaped request ("see and edit from a
non-MCP tool") and by external feedback that md reconcile is currently
one-way.

- **Optional HTTP MCP transport** alongside stdio. OAuth 2.1 + PKCE +
  Dynamic Client Registration for the multi-user case (matches the
  GBrain-style remote-MCP setup). Default install stays stdio-only.
- **`memory.json` portable format** — a one-page spec in the repo, plus
  `export memory.json` / `import memory.json`. Positions mgi-mind as a
  protocol author, not just an implementer. Tracks the proposal phase
  before claiming any cross-tool adoption.
- **Markdown live mirror (opt-in)** — keep an `~/mgimind/mirror/` tree
  in sync with Qdrant for users who want a git-able view. Mirror is a
  derived view, not a second source of truth (Qdrant remains canonical
  per the v1.0 contract).
- **Chunking upgrade**: from character-window to token-window with code-
  AST awareness on code blocks. Measured on Д6 + on a fresh long-form
  corpus.
- **Procedure auto-confirm via test signal** — when a `mind_learn` fix is
  followed by a CI / `cargo test` exit-0 in the same session window,
  promote the procedure without a separate `mind_procedure_outcome`
  call.

## v1.4 — bi-temporal facts + supersession

The "remember where I lived in 2024 vs 2026" problem, named in both the
Bedrock and the GBrain comparison docs.

- **Bi-temporal facts (Д3).** Each fact carries `valid_from` /
  `valid_until` in addition to `created_at`. `mind_fact_query` accepts
  an `as_of` parameter.
- **Fact supersession (audit #13).** When a new fact contradicts an
  existing one along the same `(subject, predicate)` axis, the old
  fact's `valid_until` is set automatically instead of producing a silent
  contradiction.
- **Contradiction surfacing.** `mgimind doctor` flags `(s, p)` pairs with
  more than one active `object`. No automatic deletion — the user
  decides.

## v1.5 — automatic decay + signal-driven consolidation

Today `mgimind consolidate` is opt-in / cron-driven. The motivation came
from both comparison docs (GBrain "dream cycles", Bedrock
"automatic decay by usage"). Builds on the existing access-counter
infrastructure (Д4) so this is wiring, not a new mechanism.

- Time-weighted decay score combining `access_count` and `last_accessed_at`.
- `mgimind consolidate --auto` is the default path; `cron` becomes the
  setup hint, not a required step.
- Decay is a ranking signal, not a delete signal. Hard delete still
  requires explicit user action through the audit-log path.

## v2.0 — public-launch gate

Not a feature dump. A gate: v2.0 = "ready for arbitrary external users".
That means the bar is **action**, not "time has passed without a CVE"
(absence of CVEs ≠ security, it means absence of attention).

Required to cut v2.0:

- External security review of the backup module from v1.2, with the
  threat-model from that release as the input document.
- Static assertion in the bucket-write path that only ciphertext leaves
  the process.
- **Self-editing memory (Д5)**, only if it can be shown to not regress
  R@k on LongMemEval-S and not increase the contradiction rate from
  v1.4. Otherwise pushed to v2.1.
- **Multi-tenant isolation (Д7)** — per-token library scoping landed in
  2.0 (`--agent-token NAME:TOKEN:lib1,lib2`, enforced on the memory
  read/write routes). Still open before the gate closes: `/memory/by-agent`
  confinement, the viewer path, and fuzz-testing the enforcement.
- Optional **encrypted collection-at-rest** with a documented key-loss
  threat model.

Anything in this block can ship earlier in a minor as a feature, but
v2.0 itself is the gated event, not just the next number.

## v3.0 horizon — candidate set, not a promise

v3.0 is far enough that committing to a single shape would be dishonest.
There are several **non-exclusive directions** the project could grow
into, each gated on signal from real users of v1.0–v2.0. The release
that gets the v3.0 tag will be whichever of these crosses both a
critic-checked spec and a measurable user pull. The others stay on this
list, or fall off it.

Candidate A — **Local LLM extractor for write-gate.** Today the
relevance gate is a heuristic stack (length, novelty, blacklist). A
small local model (mini-LLM or distilled classifier) deciding "should
this be a memory" would be the natural next step. Risk: adds a runtime
dependency on a second model; would have to stay opt-in.

Candidate B — **Judge-eval QA mode.** An opt-in path with an
explicitly-labeled paid-API mode, for like-for-like QA comparison with
e.g. Mem0 / LangMem. Numbers reported separately from the zero-API R@k
headline and clearly tagged as "+LLM cost". Closes the "but those other
systems report QA accuracy" objection without contaminating the core
metric.

Candidate C — **Cross-agent / cross-machine memory.** The REST server
from v1.3 is the substrate; v3.0-shaped scope here would be the
governance layer (per-agent scopes, per-library ACLs, a `mind_grant`
primitive) that turns mgi-mind into a multi-user company brain.

Candidate D — **Schema packs / pluggable taxonomy.** GBrain ships
"schema packs" so the user can teach the system new typed entities
(Person, Company, Deal) and the agent can evolve them at runtime.
mgi-mind today has hardcoded types (`memory`, `fact`, `procedure`).
Pluggable taxonomy would land at v3.0 only with a clear migration
story.

Candidate E — **Self-wiring graph between memories.** Today the
knowledge graph holds typed facts (S, P, O); GBrain auto-links related
markdown pages without an LLM call. A non-LLM auto-link mechanism over
memories (not just facts) is interesting but unproven on R@k.

These are deliberately listed without ordering or estimated dates. v3.0
ships when one of these is decided, scoped, critic-checked, and built —
not on a calendar.

## Anti-roadmap (things we are explicitly not doing)

Carried over from the internal roadmap unchanged:

- Obsidian plugin / live Obsidian sync. The three Obsidian-shaped
  requests are covered separately ("see" → viewer, "edit in a crisis" →
  md escape hatch in v1.0, "sync between devices" → v1.2 backup).
- Markdown as a live source of truth. Qdrant is canonical; md is
  export + reconcile + (in v1.3) opt-in mirror.
- A 50+ MCP-tool sprawl. The 31-tool v1.0 surface is the target shape.
  New tools come from a real gap, not from "we could add this".
- Cloud-hosted mode where mgi-mind sees user data. The local-first
  contract is non-negotiable through at least v3.0.
- Sales / marketing pumps for the project itself. Discoverability comes
  from publishing measurable artifacts (releases, benchmarks, an arXiv
  preprint), not from outreach campaigns.

## How this file gets updated

A release that ships a v1.x feature edits its own block to say "shipped
in vX.Y", with a link to the tag. v3.0-horizon candidates either earn a
promotion to a numbered version (with a critic-checked spec) or
silently fall off the list once the project decides against them. The
file should never grow stale promises.
