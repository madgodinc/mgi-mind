# Architecture Decision Records (ADR)

Each ADR captures one significant architectural choice and the reasoning
that led to it. Cheaper than rereading the full 42 KB synthesis document
to figure out "why is the formula shaped this way".

Order matters: 0001 is the foundation; 0004 is a reaction to 0003;
0005 is a calibration choice on top of 0003.

| # | Title | Status |
|---|-------|--------|
| 0001 | Cardinality enum instead of single_valued_predicates list | accepted |
| 0002 | Mechanism 1 invariant: never hard-delete a fact | accepted |
| 0003 | §10 q5 three guarantees on the background loop | accepted |
| 0004 | Install-mode profiles with illustrative-only anchors | accepted |
| 0005 | Superseded distinct from Stale | accepted |
| 0006 | Derived state lives in droppable side collections | accepted |

## When to write an ADR

- Any constant that goes into the public formula surface (`pub const`).
- Any change to the duel rule, doubt window, or active re-test pass.
- Any new MCP tool that mutates fact state.
- Any new payload field that v1.x readers must tolerate gracefully.

## When not to bother

- CLI flag added without changing model semantics.
- Bug fix that restores documented behaviour.
- Performance change with the same end-state.
- Build / CI / lint change.

The threshold is "will future-Mad need to remember why I did this?".
If yes, ADR. If no, the commit message is enough.
