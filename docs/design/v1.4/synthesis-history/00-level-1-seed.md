---
level: 1
status: mechanism-draft (theory pillars in place, formula open)
created: 2026-06-04
upgraded: 2026-06-04
sources_synthesised: 4 mature disciplines + 1 critic round
---

# Memory validity sort — the duel rule

> "Trust the fresh fact" is the naive rule. Every mature discipline that
> looked at this problem **abandoned it**. The replacement is not another
> signal — it is a different rule of engagement.

## The mechanism in one sentence

**A fresh contradicting fact does not replace the old one. It opens a
duel.** The old entrenched fact enters the duel with a handicap
proportional to (a) how many other memories depend on it and (b) how
often it has been confirmed on retrieval. The fresh fact wins only by
accumulating independent confirmations, not by being repeated by a
single source. The loser is not deleted — it is dampened and
timestamped.

This is the entire core. Everything below is scaffolding for the formula
Mad will build on top.

## Four pillars (each is a mature discipline pointing at the same point)

### Pillar 1 — Entrenchment ordering (belief revision, philosophy, ~1985)

Belief revision theory: each belief has an **entrenchment level** — how
deeply it is woven into the rest. On contradiction you drop the
**least entrenched** one, not the older one and not the newer one. The
fact "Mad writes Rust" is entrenched if ten other memories hang on it
(his projects, his tools, his bugs). The fact "trying Go" hangs in the
air. The duel drops the one with nothing under it.

The philosophers themselves admitted naive "trust the fresh fact" breaks
under conservative revision. This is a problem they left open. Mad is
walking into it from the engineering side, forty years later.

### Pillar 2 — Bi-temporal databases (finance, regulatory, since ~1990)

They do not mix two times:
- **valid time** — when did this become true in the world
- **transaction time** — when did the system find out

Mad's current v1.4 roadmap (was v1.3) has only valid time. The second
axis is **the critical fix** for late-arriving facts.

Example. In May the user says "by the way, I moved in March." This is
a **fresh message** (transaction time = May) about an **old event**
(valid time = March). Naive "trust the latest write" loses: it puts
this above the April-written fact, even though the April fact describes
a later real-world moment.

There is a third axis some systems carry — **decision time**: when did
the system decide to treat this as true. Useful for audit, useful for
the case "the model explained why memory believed X at moment T."

### Pillar 3 — Truth discovery / data fusion (multi-source, since ~2007)

**Counter-intuitive result, hard to copy because it is non-obvious:**
agreement between sources on an **error** is a stronger signal than
agreement on the **truth**. Truth is usually one value; errors are
many. If two sources agree on a wrong value — they are dependent (one
copied the other).

**Crucial inversion for memory:** if three different sessions
independently arrive at the same fact, that is strong confirmation.
But if three sessions all inherited it from one earlier wrong
inference by the assistant, **that is not three confirmations — that
is one, echoed**.

Almost no memory layer distinguishes these. They all count frequency.
**This is where the originality lives.** The confidence score must
separate "confirmed independently" from "echo of a single error."

This is also the technical answer to the mem0 copy fear: a formula that
weighs independence-of-confirmation is harder to clone than one that
weighs frequency, because the implementer has to understand *why* in
order to get it right.

### Pillar 4 — Memory reconsolidation (neuroscience, since ~2000s)

Evolution debugged this on humans already. Three facts, each is a hook:

1. **The brain does not erase the old on update.** New is strengthened,
   intrusion of old is suppressed, but old remains. Mad's bi-temporal
   "marked, not deleted" is architecturally already this.
2. **Retrieval strengthens.** Each time a memory is recalled and not
   contradicted, it gets stronger. This is the frequency signal — but
   refined: count not "times written" but **times retrieved without
   contradiction.** A memory that surfaced in ten answers and never
   drew a user correction is gold.
3. **Old resists harder than new.** Fresh memory is easy to overwrite,
   deeply entrenched memory is not. This gives the collision rule:
   **a fresh fact should not automatically beat an old entrenched one.**
   It must first accumulate weight through repeated confirmations.
   One offhand "switching to Go" does not flatten ten months of Rust
   — it creates a weak competing entry that either strengthens (if
   confirmed) or fades.

## Where the four pillars meet — the duel rule (expanded)

When a new fact `F_new` arrives that contradicts an existing fact `F_old`
along the same axis (same subject, same predicate, different object):

1. **Compute entrenchment of `F_old`**:
   - dependants: count of other memories that semantically rely on `F_old`
   - confirmations: count of times `F_old` was retrieved and **not** corrected
   - age-of-entrenchment: time elapsed since first independent confirmation
     (not since first write — naive age is misleading)
2. **Compute independence-weighted weight of `F_new`**:
   - is `F_new` an echo of an existing entry, or genuinely new
     observation? (sparse-token novelty, source diversity, time gap
     since last touch of that subject)
   - has `F_new` been confirmed by a second independent source? If not,
     it enters as a **candidate**, not a winner
3. **Resolve**:
   - If `F_new` weight >> `F_old` entrenchment — switch `F_old` to
     dampened state (valid_until = now), promote `F_new` to active
   - If `F_new` weight ~ `F_old` entrenchment — **both stay active as
     competing entries**, retrieval surfaces both with a "contested"
     marker, future confirmations break the tie
   - If `F_new` weight < `F_old` entrenchment — `F_new` enters
     quarantine (re-use existing quarantine layer!), promoted only on
     repeated independent confirmation
4. **Never hard-delete.** All four pillars agree on this. The loser is
   marked, dampened, timestamped — but the trace remains for audit and
   for the case where the duel reverses later.

## What stays open (Mad's territory — go here on the stims)

The four pillars give the **rule of engagement**. They do not give:

- **The formula for entrenchment.** How exactly do dependants count?
  Linear? Logarithmic? Are dependants weighted by their own
  entrenchment (recursion)? Or is depth-1 enough?
- **The formula for confirmation independence.** What makes two
  confirmations "independent"? Token novelty between the surrounding
  context? Source provenance? Time gap? Some combination?
- **The threshold for promotion / dampening / coexistence.** Where do
  the bands sit? Hard cutoffs or fuzzy?
- **The update rule for old entrenchment when a new dependant arrives.**
  When a fact gets a tenth dependant, by how much does its
  entrenchment increase? Diminishing returns?
- **The interaction with bi-temporality.** When a fact arrives with
  `valid_from < transaction_time` (late-arriving), how does the duel
  weight that? Does it skip the "freshness" presumption entirely
  because the valid-time is old?
- **Computational cost.** Entrenchment is expensive if it walks the
  full graph. Cached partial entrenchment? Background recomputation?

These are the spots where the implementation choice becomes the
contribution. Each one is a research question, and each one has a
non-obvious answer.

## Why this is hard to steal

Three layers of defense, in increasing order of strength:

1. **Git timestamp on this file** — proof of priority. mem0 cannot
   claim they had this in May 2026.
2. **Implementation in mgi-mind under Apache-2.0** — public method,
   anyone *can* copy it, but copying without understanding gives a
   broken implementation. The pillars are the explanation; without
   reading them, the duel rule looks arbitrary.
3. **The four-discipline synthesis itself** is the moat. mem0 and Zep
   live in the ML bubble. They do not read 1985 belief revision
   philosophy, finance bi-temporal folklore, or neuroscience
   reconsolidation papers. A solo developer sitting on the seam of
   four disciplines can ship something a hundred-person ML team
   structurally won't ship — not because the team is dumb, but because
   no one inside the team is paid to read all four.

This is the credit-grab defense, not patent defense. Patent comes after
the formula crystallises; for now, **timestamp + public Apache + the
synthesis story** is enough.

## Where this lands on the mgi-mind roadmap

- **v1.4** (was bi-temporal facts + supersession) — naturally absorbs
  Pillar 2 *and* Pillar 4 fact #1 (don't erase). The roadmap entry as
  written is **already half this idea**, just missing the duel rule.
- **v1.5** (decay) — naturally absorbs Pillar 4 fact #2 (retrieval
  strengthens, neglect decays). Reframe decay not as time-driven but
  as **confirmation-driven**.
- **v3.0 candidate A** (was "local LLM for write gate") — gets
  replaced or joined by **"validity duel"** as the natural v3.0
  candidate. The local-LLM angle now feels small next to this.

The roadmap text should not be rewritten until the formula is
crystallised — premature commitment to a half-formed mechanism is what
the anti-roadmap warns against. But the seed is here, timestamped, and
the next ROADMAP.md revision can lift from it cleanly.

## Critic notes that made this version stronger

Sources the critic round dropped (without me asking) and that are
worth Mad reading directly on the stims:

- **Belief revision / entrenchment**: Gärdenfors 1988, the AGM-style
  literature on "epistemic entrenchment."
- **Bi-temporal databases / XTDB**: Allen, Chen, Snodgrass — anything
  with "transaction time + valid time + decision time."
- **Truth discovery / data fusion**: search arxiv for "truth discovery
  conflicting sources" + "dependency between sources."
- **Memory reconsolidation**: PubMed for "retrieval-induced
  facilitation" + "reconsolidation overwrite resistance" + "A-B / A-C
  paradigm."

Don't read these for citation. Read them for the engineering moves
each discipline made — the moves are the contribution, the citations
will come later if Mad chooses defensive publication.

## Status

This file is **Level 1**: mechanism present, formula not. Sufficient
for prior art as the **synthesised rule of engagement** plus the four
sources. Not sufficient for a patent — the formula is still open. Sit
with it. Don't write the formula tonight unless it crystallises on its
own; the pillars will not move. Come back when something clicks.
