# ADR 0001 — Cardinality enum instead of single_valued_predicates list

**Status:** accepted.
**Date:** 2026-06-04.

## Decision

Every predicate carries a `Cardinality` enum value (`Single` /
`TemporalSingle` / `Multi`). The duel rule reads cardinality to
decide whether a second value contradicts (Single / TemporalSingle)
or coexists (Multi).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Cardinality {
    Single,
    TemporalSingle,
    Multi,
}
```

## Context

Audit #13 surfaced as PR #2 from @spikefcz — a fix for accumulating
contradictory facts on single-valued predicates. The proposed shape
was a flat `single_valued_predicates: Vec<String>` in config:
predicates on the list got auto-superseded on write.

The PR was narrow, correct for the specific case, and would have
shipped in an afternoon. It was closed superseded.

## Reasons

The flat-list approach does not generalize to three other cases the
v1.4 design needs to handle:

1. **Honestly multi-valued predicates.** "Mad uses Rust" and "Mad
   uses TypeScript" are both true. A single-valued treatment
   incorrectly flips them.
2. **Temporal single.** "Lives in Prague" was true; "Lives in
   Dublin" is true now. The old value should stay queryable as
   history (with `valid_until` set), not silently get deleted.
3. **Single-valued facts that deserve to stay live.** "Email is
   `mad@example.com`" might be a typo overwriting the real address.
   You do not want an opt-in list to silently invalidate the real
   value on the first overwrite.

The enum lets the duel rule decide what to do based on the predicate's
declared semantics, not based on the writer's order or a list of
exceptions.

## Trade-offs

- **More cognitive overhead at write time.** A new predicate needs
  to declare cardinality. `mgimind migrate-v14 cardinality` walks
  existing facts and proposes one based on observed usage; manual
  registration via `mind_predicate(action="register")` is also
  possible.
- **The proposal walk uses a heuristic** (every observed subject has
  ≤ 1 distinct object → propose Single, else Multi). High-confidence
  proposals can `--apply` directly via v1.6.3; low-confidence ones
  surface for user review.
- **Migration cost on existing bases.** Pre-v1.4 facts have no
  cardinality recorded; they read as `Multi` by default (the safe
  fallback — no false conflicts on legacy data).

## Implementation

`src/knowledge.rs`:

- `Cardinality` enum
- `register_cardinality(config, predicate, cardinality)` — write to
  the cardinality registry.
- `lookup_cardinality(config, predicate)` — read with `Multi` default.

`src/migrate_v14.rs::run_cardinality_inference`:

- Scrolls the facts collection.
- Per predicate, counts distinct objects per subject.
- Emits a JSON file: `{predicate: {proposed, confidence, reason}}`.
- `--apply` calls `register_cardinality` for every High-confidence
  entry.

`src/duel.rs::resolve_against_existing`:

- Checks `cardinality.admits_conflict()` before triggering the duel.
- Multi → write both, return Contested-zero.
- Single / TemporalSingle → run the entrenchment-vs-weight contest.

## Alternatives considered

- **`single_valued_predicates: Vec<String>` (PR #2).** Rejected as
  described above.
- **Inferred from usage at read time.** Rejected — the duel rule
  needs to run at write time for the loser to be dampened
  atomically with the winner being written.
- **Subject-level cardinality** (different subjects can have
  different rules for the same predicate). Considered for v2.0;
  not in scope for v1.x.

## Future revisions

If the heuristic walk repeatedly misclassifies predicates in a way
the user has to override, v1.7 may add per-subject overrides or
soft / hard distinction (soft = warn on conflict but allow,
hard = always conflict). Not driven by demand yet.
