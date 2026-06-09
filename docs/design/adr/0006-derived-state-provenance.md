# ADR 0006: Derived state lives in droppable side collections

**Status:** accepted.
**Date:** 2026-06-09.

## Decision

State written into the store falls into two kinds, and they live in
different places:

- **Raw state**: what the user (or an agent) authored: memories, facts,
  procedures. Lives in the core collections (`memories`, `_kg_facts`,
  `_kg_predicates`).
- **Derived state**: what a feature *computed* from raw state: outcome
  counters, access/decay counts, validity verdicts. Should live in its
  own side collection, prefixed `_mod_*`, keyed by the core point's id.

The invariant on derived state:

> Dropping a derived (`_mod_*`) collection must leave the system in a
> **correct, if less refined, state, never a worse one than core
> without that feature.** No derived feature may hard-delete raw state.

The rule keys on what kind of data a field is (raw or derived), not on
which code wrote it. A "module may never write to core" rule sounds
equivalent and is the right default, but it breaks on two cases the
provenance framing handles. Context covers them.

## Context

The codebase already half-obeys this split. Mad surfaced it with one
question: *what survives when you turn a feature off?*

Two failure modes motivate the rule:

1. **Behavior is testable; transitions are not.** A test suite proves
   each feature works with all features on (one point in a 2^N config
   space). It does not prove that data written in one config is read
   *correctly* in another. The bug lives in the transition between modes,
   and it lives in the data, not in either mode's code. Example: a feature writes
   `propagation_shadowed` onto a core fact, then the feature is turned
   off; its code now correctly does nothing, but the read path still
   hides that fact by a status no code can set or clear. Both modes are
   green in isolation; the transition between them is broken. A full
   feature-run sweep cannot catch this: it tests function behavior, and
   this is a state-change scenario.

2. **A precedent already proves the clean shape works.** `access.rs`
   keeps decay/access counts in an in-process map plus a small journal,
   explicitly *not* on the core point. The search hot path notes it is
   "NOT a Qdrant write on the read path." Drop that store and the system
   reverts to correct (un-decayed) behavior with no ghost. That is the
   target shape for derived state.

### Why "a module may never write to core" is the wrong precise rule

- **Too strong for hot-path filters.** Quarantine hides junk via a
  server-side `must_not quarantined=true` on *every* search; validity
  hides stale facts the same way. Qdrant has no joins, so moving those
  verdicts to a side collection means: enumerate the module's id-set
  (an extra round-trip), then pass it as a server-side `must_not
  has_id([...])` filter. `has_id` exists, so the filter still runs
  server-side, but the id list is unbounded and the quarantine set
  *grows with junk*, so that filter gets heavier precisely as the store
  gets dirtier. The current core-stamped flag is one indexed boolean
  with no enumeration round-trip. Keeping it is the performant choice.
- **Too weak for permanent cleanings.** If quarantine's verdict were a
  freely-droppable module, turning it off would resurrect ~10k junk
  memories into search. That is a regression dressed as a free toggle.
  Quarantine is a *cleaning you want permanent in effect*, even though
  you'd like its code to be toggleable. "Drop the module, its effect
  vanishes" is the wrong durability for exactly that feature.

The provenance rule handles both. It lets a load-bearing verdict stay
core-stamped while it is reversible (soft-flag, never hard-delete) and
the raw fact is reconstructable. What it forbids is stamping
**non-reconstructable** derived state onto core, where raw and computed
blur together and you can no longer undo the computed part.

## The cut line

When a feature is sliced as a candidate side module, classify its
derived state:

- **Derived AND not on the global hot path** → side collection
  (`_mod_*`). Cheap and clearly correct. Examples: procedure outcome
  stats (rank only inside `mind_recall`), access/decay counters
  (already clean).
- **Derived AND a load-bearing hot-path filter** → stays core-stamped,
  under the reversibility invariant (soft-flag, never hard-delete).
  Examples: quarantine (`quarantined`), validity statuses (`stale`,
  `superseded`). Converting these costs hot-path latency that scales
  with store dirtiness and buys nothing for a single-user store whose
  features live on; defer indefinitely unless a real multi-config need
  appears.

Raw user-authored state never moves out of core.

## Consequences

**Plus:**

- New features have a default home for computed state (`_mod_*`), so the
  next outcome-counter or score doesn't get stamped onto a core point by
  habit.
- The 2^N transition class shrinks: anything in a `_mod_*` collection is
  drop-safe by construction (drop the collection, the effect is gone, no
  stale verdict survives on a core point). The reverse direction (delete
  a core point, orphan its `_mod_*` row) is a separate consistency
  question handled under Open Questions, not a ghost on the read path.
- The rule is a one-line discipline for new code, not a refactor: "is
  this raw or derived? if derived and not a hot-path filter, side
  collection."

**Minus:**

- Two homes for derived state (core-stamped reversible vs side
  collection) instead of one. The cut line above is the rule for which.
- A side-collection read costs one extra `get_points` by id-set at rank
  time. Bounded by `limit` (O(k)), not by store size, so it does not
  reintroduce the join cost that keeps hot-path filters in core.

## First application

Procedure outcome stats (`success_count`, `fail_count`, `verified`,
`last_used`) move from the core procedure point into `_mod_procstats`.
They are derived (computed from replay outcomes) and not on the global
hot path (they only rank inside `mind_recall`, which already re-ranks in
Rust after fetch). Dropping `_mod_procstats` must leave `mind_recall`
returning every procedure correctly, just without the trust boost. That is
the toggle-test that proves the invariant on real data, with the blast radius
of one function and zero risk to the benched search path.

That graceful-degrade is an implementation obligation, not a free
property: a bare `get_points` against a dropped collection returns an
error in Qdrant. The side read must be existence-guarded and
default-on-miss, the same pattern `existing_procedure` already uses
(`let Ok(resp) = … else { return defaults }`), yielding
`success_count = 0, fail_count = 0, verified = false` so `rank_score`'s
boost is simply zero. Get that wrong and a missing collection throws
instead of degrading, falsifying the toggle-test. See the companion
implementation.

## Open questions (deferred)

- Whether to ever convert quarantine/validity to side collections. Per
  the cut line, probably not: their verdicts are load-bearing hot-path
  filters and a join-less side collection makes search scale with junk.
  Revisit only if a genuine multi-config (per-tenant, A/B) need appears.
- Whether `_mod_*` collections need their own retention/GC, or whether
  they ride the core point's lifecycle (delete the core point, leaving
  its derived rows orphaned). For `_mod_procstats` the orphan is
  harmless (a stat with no procedure is never read); revisit if a side
  module ever holds large per-point state.

## References

- Code: `src/access.rs` (the clean precedent), `src/storage.rs`
  (`procedure_outcome`, the search hot path, `create_vectorless_collection`),
  `src/knowledge.rs` (`EntryStatus`, the core-stamped validity statuses).
- Companion ADRs: [0002](./0002-mechanism-1-invariant.md) (never delete),
  [0005](./0005-superseded-vs-stale-status.md) (core-stamped statuses).
