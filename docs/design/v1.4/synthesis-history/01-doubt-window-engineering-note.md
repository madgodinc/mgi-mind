---
level: 1
status: engineering note for v1.5 decay
created: 2026-06-04
relates: [[2026-06-04-memory-validity-sort]]
---

# Doubt window for entrenched facts

## The problem the duel rule creates

The duel-rule mechanism gives an old, well-entrenched fact a handicap
proportional to its weight. Under any reasonable formula, an entrenched
fact wins almost every duel by default — that's the point. But this
mechanism has a dark side that needs to be designed-in from the start,
not bolted on later when symptoms appear:

**An entrenched fact resists correction proportionally to how well
entrenchment works.** A fact that has been confirmed twenty times
across six months, with eight other memories hanging on it, will beat
almost any single fresh contradicting fact. If the fact has *quietly
gone stale* — true once, no longer true — the system has no built-in
mechanism to discover this. It just keeps winning duels on accumulated
weight.

This is the same failure mode as human stubbornness: the brain doesn't
resist correction because the old belief is more correct, it resists
because the belief is older and has accumulated more entrenchment. The
mechanism that makes memory wise (don't flip on every fresh contradiction)
is the same mechanism that makes it ossify.

In v1.4 + v1.5 as currently scoped, decay handles "fade if never
retrieved" but does nothing for "retrieved often, never re-tested."
That second case is the exact failure mode the duel rule amplifies.

## The mechanism

Borrow from the neuroscience pillar: **retrieval does not strengthen a
memory unconditionally. It strengthens it only if the retrieval
*context* still matches the memory.** Mere recall without context-
check is not confirmation.

Translate to mgi-mind:

1. **Define a doubt threshold per fact.** A function of entrenchment:
   the *more* entrenched a fact, the *more frequently* it must
   re-justify itself against fresh evidence. Counter-intuitive but
   correct — the system should be *more*, not less, demanding of its
   strongest beliefs.

2. **On retrieval of a highly entrenched fact, check the surrounding
   context for compatibility.** If the current session's context
   contains tokens / facts that contradict or do not match the
   retrieved fact's context-of-origin, *don't strengthen on this
   retrieval*. Optionally, mark as "retrieved-but-not-confirmed."

3. **After N retrievals-without-confirmation, the fact enters a
   "doubt window"** — its entrenchment weight is temporarily reduced
   so that even a moderately weighted fresh contradiction can win the
   next duel. The fact has to fight again to re-earn its position.

4. **Doubt is not decay.** Decay is "fade if neglected." Doubt is
   "test what you most rely on." A fact in the doubt window is not on
   the way out — it is being asked to prove itself one more time. If
   it wins fresh confirmations, entrenchment resumes; if it loses,
   the duel resolves normally.

## Why this matters as a v1.5 design constraint

Without this: the system gradually develops "wisdom" that is
indistinguishable from prejudice. Long-running mgi-mind installs will
have a class of facts that are wrong but unkillable. Users will work
around it by manually invalidating, which is the worst of both worlds
(manual labor on what was supposed to be automatic memory).

With this: the system stays self-correcting at exactly the place where
it would otherwise be most confident. The expensive operation
(re-test of entrenched facts) is cheap to amortise because entrenched
facts are by definition the ones most often retrieved — the
re-check happens in the same path as the retrieval that would have
strengthened them anyway.

## Open questions (where the formula will live)

- Doubt threshold as a function of entrenchment: linear? Square root?
  Step function above a hard threshold?
- Context-compatibility check: token overlap with origin context? Same
  subject-predicate axis stability? Semantic similarity of surrounding
  memories at retrieval time vs at write time?
- Doubt window size N: fixed? Adaptive per fact?
- Interaction with quarantine: does a fact that lost during the doubt
  window go to quarantine, or just back to candidate-level?

## How this lands

Lift this note into the v1.5 ROADMAP entry when v1.5 starts. Don't
edit the public roadmap yet — the duel rule formula isn't crystallised
either, and the doubt window only makes sense as the counterweight to
that mechanism.

If the duel rule never crystallises, this note still has value on its
own: a built-in re-justification check on highly-retrieved facts is
the kind of feature that, after it ships, sounds obvious in hindsight.
The four-pillar synthesis just made it findable.
