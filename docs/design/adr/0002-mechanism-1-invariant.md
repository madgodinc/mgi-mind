# ADR 0002 — Mechanism 1 invariant: never hard-delete a fact

**Status:** accepted.
**Date:** 2026-06-04.

## Decision

No code path in mgi-mind removes a fact from the facts collection
based on duel-rule logic, doubt-window logic, or active-re-test
logic.

The loser of a duel is **dampened** — payload status moves to
`Stale`, `valid_until` is set. The fact stays readable; it's just
hidden from default ranking.

The `RetestTransition` enum that drives the v1.5 Phase 8 re-test
pass is exhaustive **without** a `Remove` variant:

```rust
pub enum RetestTransition {
    NoChange,
    PromoteToDoubt,
    RecoverFromDoubt,
}
```

If a contributor adds a fourth variant, the type system catches
it. There's no "and also delete" arm to slip in.

## Context

Validity-model synthesis §3 mechanism 1. Came out of mass-rejection
behaviour in early-2026 prototype designs that auto-superseded
conflicting facts — when a flip turned out to be wrong (typo, bad
inference from extractor), there was no way to recover the original.

## Reasons

Three failure modes that hard-delete cannot recover from:

1. **Future evidence can reverse the duel.** Confidence might drop
   below threshold for the wrong reason today; six months later a
   re-test on better data could promote it back. Hard-delete makes
   the recovery impossible.

2. **Audit trail.** When the model behaves unexpectedly months
   later, grep on the audit log to find the moment a fact flipped
   and the score numbers at the time. Hard-delete leaves only the
   gap; no signal that the fact ever existed.

3. **STALE benchmark behavioural metrics.** `state_resolution` and
   `premise_resistance` measure whether the system can correctly
   reject a stale claim. They need the loser still readable for the
   judge to see how the duel resolved. Hard-delete silently zeroes
   the metric.

## Consequences

- **Payload size grows.** Stale facts stay on disk. Acceptable —
  Qdrant payload is cheap, and quarantine / consolidate paths
  exist for genuinely cold data.

- **Search needs to filter status.** Default-visible facts are
  Active / Contested / Unknown; Stale / PropagationShadowed are
  hidden unless `include_stale=true`. This is done at the search
  layer, not by erasure.

- **Soft-decay must go through quarantine.** The only "remove from
  active service" path goes via `consolidate --soft-decay` →
  quarantine → revive on demand. v0.11 quarantine API handles this.

## Implementation

`src/duel.rs::DuelOutcome`:

- `Flip` — winner becomes Active; loser dampened to Stale.
- `Contested` — both stay live with `EntryStatus::Contested`.
- `Quarantine` — loser enters QuarantineCandidate state (waiting
  for promote-on-repeat).

`src/duel.rs::dampen_loser`:

- Sets `valid_until` to current time.
- Sets status to Stale.
- Writes to audit log.
- Does NOT call any delete API.

`src/confidence.rs::decide_retest_transition`:

- Exhaustive match on three variants only.

`src/doubt.rs::retest_fact_step82`:

- Calls write paths for the resulting transition.
- Never calls `storage::delete_fact`. Per ADR 0003 there is no
  hard-delete in the background loop.

## Trade-offs

- **Storage cost.** Every fact ever written occupies payload.
  Mitigation: quarantine + soft-decay.
- **Search cost.** Filter-by-status on every query. Mitigation:
  Qdrant index on status field; cheap.
- **No way to delete a fact written by mistake.** True — but
  `mgimind fact invalidate <id>` exists and marks it Stale. Real
  hard-delete is only via `mgimind delete` which goes through the
  audit log and is intended for user-initiated content removal,
  not duel-rule resolution.

## Alternatives considered

- **Hard-delete the loser on a Flip.** Rejected — failure modes
  above.
- **Time-based hard-delete after N months stale.** Rejected for
  v1.x — same risks as immediate hard-delete, deferred. v2.0 may
  add a configurable retention policy that hard-deletes Stale
  facts older than N years; the duel rule itself stays soft.

## Tests pinning the invariant

- `confidence::tests::never_returns_delete_verdict` — 200-point
  grid over `(old, new, in_doubt)` asserts `decide_retest_transition`
  never produces a variant other than the three listed.
- `doubt::tests::busy_flag_observable_by_loop_check` — exercises
  the BusyGuard pattern.

If a contributor PRs a fourth `RetestTransition` variant, the test
above does not break, but the exhaustive `match` in
`retest_fact_step82` does — Rust catches the bypass at compile time.
