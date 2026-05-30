# Phase Д2 / Д6 — Auto-memory & Procedural memory

Design doc for auto-extraction (Д2) and procedural / error→fix memory (Д6),
built on top of the post-phase-0 single-process foundation (v0.8). This is the
**finalized** spec — agent-driven default, decay via in-process counters,
error→fix gated on a truth signal — not the first naive draft.

## Why now (prerequisites already closed by phase 0)

- **Daemon concurrency was the blocker.** Single-process MCP removed it: for one
  user, inference is sequential and that is enough — no session pool needed.
- **0.8 shipped reusable bricks:** vectorless payload-indexed facts
  (`knowledge.rs`, the template for procedures), the sparse branch
  (`storage::sparse_vector`/`token_id` — ideal for error signatures),
  deterministic IDs (`storage::deterministic_id` — dedup). We start far from zero.

## Decisions locked in (do not revisit without Mad)

- **Delivery: PR-by-PR.** Foundation → consolidation → auto-ingest → procedural.
  Each branch reviewed and merged by Mad. Easier to verify the two invariants.
- **Decay: in-process access counters + periodic journal flush.** Counters live in
  the long-lived `mgimind mcp` process; reads stay vector-read-only (honors audit
  #5 — no write-on-read). Flushed to a small journal file, decoupled from vectors.
- **Consolidation trigger: `mgimind consolidate` CLI (+ cron).** Does NOT enter the
  hot single-process read loop (where panic-isolation is safety-critical).
  Background-on-idle inside the MCP process may be added later as an option.
- **Auto-ingest judgment is pluggable; agent-driven is PRIMARY** (inverted from the
  first draft). Heuristics are a backstop, BYO-LLM is opt-in/off-by-default.
- **error→fix proactivity is gated on a truth signal** (test green / exit 0),
  which an external harness/verification-gate supplies — not mgimind.

## Two hard invariants (enforced as tests / gates)

1. **No auto-write before consolidation.** Auto-ingest without consolidation =
   store bloat → recall degradation. Do not ship the ingest PR before the
   consolidation PR has landed.
2. **No proactive `verified` without a truth signal.** error→fix that learns from
   un-verified fixes learns superstition (correlation, not causation). Only
   `verified=true` is surfaced proactively; unverified is low-weight.

---

## Д2 — Auto-extraction

System extracts memory from the stream (turns, errors, decisions) instead of
manual `mind_add`.

**Where judgment lives — hybrid.** Server gives mechanics; judgment is a pluggable
layer. Mode priority:

1. **Agent-driven (primary).** The agent is already a frontier LLM in the loop; it
   calls `mind_ingest` with already-extracted candidates. This *is* "local
   judgment, no cloud," and it is the strongest mode.
2. **Heuristics (backstop).** For raw turns / non-agent clients (dumb client pastes
   a transcript): markers `remember/always/never/my X is`, decisions, error+fix.
   Catches ~20% without judgment — so it is a backstop, not the default.
3. **BYO-LLM (opt-in, off by default).** Local small model or external API.
   Off by default or we break the LLM-free identity.

**Pipeline (5 stages):**

1. **Capture** — `mind_ingest(raw)` accepts raw input, stages it. Does NOT write
   verbatim into searchable memory (noise).
2. **Extract** — pluggable Extractor → candidates of three types:
   `memory` / `fact` / `procedure`.
3. **Dedup/merge** — exact via deterministic ID (have it); near-dup via top-1
   cosine ≥ threshold. This near-dup helper is the still-missing audit #8.
4. **Gate** — significance threshold. Honestly: gate quality = extractor quality.
   In agent mode the agent IS the gate (only sends what is worth it) — so #1
   partially dissolves this problem.
5. **Consolidate (background)** — merge near-dup, decay rare, summarize clusters.

**Two mandatory companions to auto-write (without them: worse than today):**

- **Consolidation** — cannot be deferred. Auto-ingest without it bloats the store
  → recall degrades.
- **Secret scrub** — critical. Auto-ingest will suck in `.env`, keys, passwords.
  A secret detector runs BEFORE any write → route to vault or drop, never into
  searchable memory.

**Decay decision:** `access_count` is not in the payload, and incrementing on read
= write-on-read, which conflicts with audit #5 (reads are read-only). So: keep
access counters in process memory, decoupled from the vector, periodically flushed
to a small journal. Single-process makes this natural.

**Data:** add a `type` field (`memory` / `fact` / `procedure`) + index, to filter
by type within the single collection.

---

## Д6 — Procedural memory ("learning from screw-ups")

Playbooks of "how we fix / do this," primarily error → fix. A special case of
extraction + retrieval at task time.

**`procedure` record:**

```
type: "procedure"
trigger_error:   "<normalized error signature>"  // no line numbers / paths / hashes / addresses
trigger_context: "<short task description>"
fix:             "<what worked>"
provenance:      "<project / file>"
verified:        true | false      // did it pass a deterministic check
success_count, fail_count, last_used
```

**Retrieval.** Normalize the error signature (strip line numbers, paths, hashes) →
lexical/sparse match (exact error codes & identifiers are caught by the sparse
branch we already have — nearly free) + dense over task context. Surface the top
playbook when the agent hits an error or starts a similar task.

**Dependence on a truth signal (fundamental, not an implementation bug).** Without
a "the fix actually worked" signal you learn superstitions — correlation, not
causation. A reliable `verified=true` needs a deterministic signal (test green /
exit 0) reported by the harness / a separate verification-gate project, not by
mgimind. Therefore:

- **MVP shipping now:** manual `mind_learn(error, fix, verified=false)` — the agent
  explicitly records the lesson.
- **Reliable mode:** a hook on the verification signal → auto-mark `verified=true`.
  Tied to external machinery that does not exist yet.
- **Proactivity rule:** only `verified` is surfaced proactively; unverified is
  low-weight. On reuse: a surfaced fix fails again → `fail_count++` → demote. So
  memory self-corrects rather than ossifying on a bad fix.

---

## Build order (updated for post-phase-0)

| PR | Contents | Notes |
|----|----------|-------|
| — | **Concurrency** | Already removed by phase 0 (single-process). |
| **PR1** | near-dup helper (top-1 cosine, missing #8) + `type` field & index + decay decision (in-process counters + journal) + **secret scrub** | Foundation. Secret scrub wired into `add_memory` immediately. |
| **PR2** | Consolidation (dedup/merge/decay) via `mgimind consolidate` CLI | **Before** any auto-write (invariant 1). |
| **PR3** | Auto-ingest MVP — agent-driven primary, heuristics backstop, `mind_ingest`, dedup. No LLM. | |
| **PR4** | Procedural memory — `procedure` record + error-sig retrieval, manual `mind_learn` first. | |
| later | Auto error→fix — hook on verification signal (external dependency). | Invariant 2. |
| later | Opt-in BYO-LLM extractor. | |

## Two places where "unfinished" = "worse than before" (not "fewer features")

1. Auto-write without consolidation → bloat → recall degradation.
2. error→fix without a truth signal → ossifying on superstitions.

These are hard invariants: do not ship PR3 without PR2, and do not present the
error→fix auto path as working until the external signal exists.
