# v1.4 — validity / relevance model

Design docs for v1.4. Relevance moves from `semantic_match × recency`
to a multi-axis calibrated belief weight.

## Files

- `synthesis.md` — what we build and why. 4 mechanisms: duel rule,
  doubt window, inheritance discount, bi-temporal axes.
- `implementation-plan.md` — how, in what order, with gates. 5 phases.
- `synthesis-history/` — earlier drafts and engineering notes.

## Status

- Synthesis went through 2 critic rounds. Changelog in `synthesis.md` §7.
- Implementation via branches `v1.4/phase-N-...`.
- Current: `v1.4/phase-0-schema-primitives` — Cardinality enum +
  confidence_score field, 13 unit tests, no behavior change to v1.1.

## Privacy

Code, design docs, public-corpus benchmarks → repo.
Author's real memory data → local only, never committed.

## Prior art trail

Earlier drafts in private `~/Brain/ideas/` (commits `6fef735` →
`92fe34b`, 2026-06-04 03:00 → 05:50 UTC). Public synthesis is FINAL;
hashes in `synthesis.md` §7 for verification.
