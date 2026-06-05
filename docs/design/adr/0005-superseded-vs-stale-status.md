# ADR 0005 — Superseded distinct from Stale

**Status:** accepted.
**Date:** 2026-06-05.

## Decision

`EntryStatus` has two distinct hidden-from-default-query states:

- **`Stale`** — written by `dampen_loser` when a fact lost a contradiction
  duel. The fact's claim was wrong (or at least less load-bearing than
  the winner's); the audit log records who beat whom.
- **`Superseded`** — written by `mark_superseded` when a fact in a
  `TemporalSingle` chain is overtaken by a newer entry. The fact's claim
  was correct at its time; it stopped being current.

Both are hidden from the default read path. Both keep `valid="true"`
and a `valid_until` timestamp. Both can be surfaced by audit / history
tools. The distinction lives in how those tools render them:

- `mind_history` should return `Superseded` entries as part of a normal
  time-ordered answer to "what was X on date Y."
- The audit log should render `Stale` entries with the duel outcome that
  produced them (winner id, weights at time of duel).

## Context

The original v1.4 design collapsed both into `Stale`. The motivation
for revisiting:

1. **Mechanism 1 invariant says never delete.** Good. But "never delete"
   alone doesn't tell the user *why* a fact is no longer current. A user
   debugging their agent's behavior wants to know "was this overwritten
   by a contradiction, or by the passage of time?" — those are different
   stories.
2. **The `redo-duels` walk surfaces both shapes.** When we run the walk
   on a real KG, it finds `Aurora has_status active` (overwritten by
   contradiction — Aurora got frozen) and `mgi-mind has_version v0.8.0`
   (overtaken by time — v1.6.4 is current). Both must be hidden from
   default queries; the user-facing meaning differs.
3. **STALE benchmark behavioural metrics need the distinction.** Several
   of the metrics distinguish "premise rejection" (Stale) from "temporal
   update" (Superseded). Folding both into one state silently makes those
   metrics undefined.

A boolean would have worked too (`is_superseded: bool` on top of `status`).
We chose the enum variant because:

- `EntryStatus` is already the typed state model; adding a boolean would
  introduce two parallel state systems.
- Read-path filters express both exclusions in one place
  (`must_not status in {stale, superseded}`).
- Future states (e.g. `Quarantined`, `PropagationShadowed`) compose into
  the same enum; a boolean wouldn't.

## Wire format

Status field on the fact payload, lowercase strings:

```
active                # default-visible
contested             # default-visible
unknown               # default-visible (legacy / migrating data)
stale                 # default-hidden, lost a duel
superseded            # default-hidden, overtaken by time
quarantine_candidate  # default-hidden, weak rejected
propagation_shadowed  # default-hidden, inherited from a parent that flipped
```

The parse helper accepts each variant in lowercase + a few common
aliases (`shadowed`, `candidate`). New variants append, never rename.

## Consequences

**Plus:**

- A future `mind_history` tool has a clean filter
  (`status="superseded"` for the chain, `status="active"` for the
  current value).
- Audit replay can render the right diagnostic per state.
- The `redo-duels` walk has a typed verb per cardinality
  (`dampen_loser` for Single, `mark_superseded` for TemporalSingle).
- Tests can assert the right state was written, not just "the fact is
  hidden somehow."

**Minus:**

- Two helpers in `duel.rs` (`dampen_loser`, `mark_superseded`) that look
  nearly identical. We chose duplication over a `dampen_with_status` API
  because the call sites (duel-resolution path vs temporal-supersede
  walk) read better with the specific verb.
- Every read-path filter has two `must_not` conditions instead of one.
  Costs a tiny amount of query specificity in Qdrant; the lost speed is
  negligible compared to the embedding inference path.

## Open questions (deferred to v1.8+)

- Should `Superseded` carry a `superseded_by: fact_id` payload field
  for explicit chain pointers? Currently the chain is reconstructed via
  `(subject, predicate)` grouping at query time. An explicit pointer
  would help `mind_history` produce ordered answers in one Qdrant call.
- Should the duel rule produce a `Supersede` outcome for TemporalSingle
  conflicts that arrive in arrival order, distinct from `Flip` for
  Single? Currently both flow through `Flip` and the cardinality is
  read again at write time to pick the loser-marking helper. A typed
  outcome would centralize the decision.

## References

- Code: `src/knowledge.rs` (`EntryStatus`), `src/duel.rs`
  (`dampen_loser`, `mark_superseded`), `src/cli.rs` (`RedoDuels` walk).
- Issue: [#27](https://github.com/madgodinc/mgi-mind/issues/27).
- Companion ADRs: [0001](./0001-cardinality-enum.md),
  [0002](./0002-mechanism-1-invariant.md).
