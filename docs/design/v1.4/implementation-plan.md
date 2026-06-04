---
status: implementation plan for the v1.4 validity/relevance model
created: 2026-06-04
depends_on: [[2026-06-04-FULL-SYNTHESIS-for-critic.md]]
target_release: mgi-mind v1.4
---

# Implementation plan — validity/relevance model

The synthesis (`2026-06-04-FULL-SYNTHESIS-for-critic.md`) says **what**
we are building and **why**. This document says **how** and **in what
order**, with explicit gates between steps. The gates are what keep
the work from drifting into "wrote formula, doesn't work, don't know
why."

The rule for every gate: you cannot start the next step until the
previous one has produced a measurable artifact that answers the
question the next step needs.

---

## Phase 0 — pre-work (1 evening, ~2 hours)

Goal: lay down primitives that all later phases assume. No data
required. No formulas. Pure scaffolding.

### Step 0.1 — Cardinality registry (45-60 min)

**Artifact:** `enum Cardinality { Single, TemporalSingle, Multi }` +
optional `cardinality` field on knowledge-graph predicate types +
`mind_fact_add(..., cardinality: Option<Cardinality>)` with default
`Multi` for unknown predicates.

**Why first.** §4 of the synthesis is load-bearing for everything
downstream. Duel rule (Phase 2) cannot fire correctly until predicate
cardinality exists. Migration (Phase 1) needs to know which axis to
emit cardinality recommendations on.

**Tests:**
- Adding two facts with same `(subject, predicate)` for `Multi`
  predicate → both stored, no conflict event raised.
- Adding two facts with same `(subject, predicate)` for `Single`
  predicate → conflict event raised (resolution comes later in
  Phase 2; for now, just the event).
- Unknown predicate defaults to `Multi`, prints info log "predicate X
  registered as Multi (default)."
- `mind_fact_add(..., cardinality=Some(Single))` registers the
  predicate as `Single` for all future uses.

**Gate to Phase 1:** all four tests green, conflict events visible in
`mgimind doctor` output.

### Step 0.2 — Confidence score field skeleton (15-20 min)

**Artifact:** `confidence_score: Option<f32>` payload field on every
memory in Qdrant, defaulted to `None` for legacy memories, defaulted
to `0.5` for new ones written after this commit.

**Why.** §6 of synthesis needs a place to put the cached score. We
add the field now (zero compute), populate it later in Phase 3 once
the formula is calibrated.

**Tests:** field round-trips through `mind_add` and `mind_search`,
serialization stable.

**Gate to Phase 1:** field exists, deserialization of existing 12k
base does not break (defaulted-None is forward-compatible).

---

## Phase 1 — migration + measurement (3-5 days)

Goal: turn the existing 12k-memory base into a substrate the rest of
the work can run against. **Most important phase**. Phase 2-4 formulas
are calibrated against the distributions discovered here. Skipping
this phase or rushing it is what produces "compiled formulas that
don't actually work."

### Step 1.1 — Dependants backfill (1-2 days)

**Artifact:** a one-shot `mgimind migrate dependants` CLI command
that walks the existing knowledge graph and, for every fact, counts
how many other memories semantically depend on it.

**Definition of dependant** (the operational one for v1.4): memory M
is a dependant of fact F if M's content semantically references F's
subject OR F's object with cosine ≥ 0.7 against F's combined
`(subject, predicate, object)` vector.

**Why this threshold.** 0.7 is the standard "definitely related" line
in MiniLM space at our chunking. It is conservative — we want to
under-count, not over-count, because over-counting inflates
entrenchment in a way that helps no one. Calibration may revise this
in Phase 4.

**Output:** per-fact `dependants_count` payload field. Distribution
histogram printed at end (`min / p10 / p50 / p90 / max`).

**Gate to Step 1.2:** the distribution histogram. We need to see
whether dependants are concentrated (fat tail), uniform, or sparse.
This determines what shape the entrenchment formula in Phase 2 takes.
*Linear vs logarithmic is not a theoretical choice — it is a choice
the data dictates.*

### Step 1.2 — Cardinality inference (1 day)

**Artifact:** `mgimind migrate cardinality` walks every existing
predicate in the knowledge graph and proposes a cardinality based on
observed usage:

- predicate used only ever with one distinct object per subject across
  all subjects → propose `Single` or `TemporalSingle`
- predicate used with multiple distinct objects per subject for at
  least 20% of subjects → propose `Multi`
- ambiguous → propose `Multi` with a "review" tag

**Why this matters.** Without cardinality, the duel rule fires
nowhere (default Multi for all). With overly-aggressive Single
defaults, the duel rule fires everywhere, including false conflicts.
The migration choice **sets the calibration baseline** for the entire
duel-rule behavior on the existing base.

**Output:** `cardinality_proposals.json` for the user to review. User
accepts, edits, or rejects per predicate. Accepted proposals write to
the cardinality registry from Step 0.1.

**Gate to Step 1.3:** the proposals reviewed and committed. This is a
**human-in-the-loop step by design**. Auto-applying cardinality
proposals would be the exact "recalibrate someone else's beliefs
without consent" failure mode named in §10 question 6.

### Step 1.3 — Confirmation backfill (where possible) (0.5-1 day)

**Artifact:** for memories that already have a derived confirmation
signal (linked to `mind_procedure_outcome(worked=true)`, linked to
provenance with multiple distinct origin URLs), populate
`confirmations_count`. For everything else, leave at 0. Print
distribution.

**Why partial.** Most legacy memories have no confirmation history
that can be backfilled honestly. Forging confirmations would
contaminate calibration. Leave them at 0 and let confirmations
accumulate from this point forward.

**Gate to Phase 2:** distribution printed. We see what the
"confirmed" subset looks like vs "unconfirmed" — this determines
whether the §6 single-user weight w_confirmations ≈ 0.1 is correct
or needs to drop further (if very few facts have confirmations,
giving them weight at all is over-engineering for current data).

### Step 1.4 — Smoke regression bench (0.5 day)

**Artifact:** re-run LongMemEval-S R@k on the migrated base to
confirm the migration itself didn't regress retrieval. Should be
identical (we only added payload fields, didn't change ranking yet).

**Gate to Phase 2:** R@k matches v1.1 baseline within ±0.5pp. If it
doesn't, the migration broke something; fix before continuing.

---

## Phase 2 — duel rule (3-5 days)

Goal: implement Mechanism 1 with formulas calibrated to the
distributions from Phase 1.

### Step 2.1 — Entrenchment formula v0 (1 day)

**Artifact:** a function `entrenchment(fact) -> f32` that combines
dependants, confirmations, and age into a single score. **Formula
shape chosen by Phase 1 data**, not a priori.

Default starting point if data permits a choice:
- dependants contribution: `log2(1 + dependants_count)` if fat-tail
  distribution; linear in dependants if uniform
- confirmations contribution: per §6 install-mode weights
- age contribution: months since first dependant added (not since
  fact written) — to avoid the v1 "naive age" trap

**Calibration:** the function output is normalized so that the median
entrenchment in the migrated base is 0.5, the 90th percentile is 0.8,
the 99th percentile is 0.95. This makes thresholds in Step 2.2
interpretable.

**Tests:** synthetic facts with hand-picked dependants/confirmations
produce expected ordering. Real-data spot-check on 10 known-high-
entrenchment facts (e.g. "Mad writes Rust") vs 10 known-low-
entrenchment facts (one-off mentions).

**Gate to Step 2.2:** entrenchment ordering matches author intuition
on the 20 spot-check facts. If it doesn't, the formula needs to
change before any threshold is set.

### Step 2.2 — Duel resolution (1-2 days)

**Artifact:** when a conflict event fires (Step 0.1 already
detects them, Step 1.2 made cardinality real), run the duel:

```
weight_new  = f(inheritance_discount, in-session_diversity, bi_temporal_stance)
entrench    = entrenchment(F_old)

if weight_new > entrench × 1.5:  flip
elif weight_new > entrench × 0.5:  contested
else: quarantine F_new as candidate
```

The constants `1.5` and `0.5` are starting points. They will move
in Phase 4 calibration. Document them as `DUEL_FLIP_RATIO` and
`DUEL_CONTESTED_RATIO` in code so they are findable.

**Tests:**
- F_new with high external-signal weight beats high-entrenchment
  F_old (the "cargo test exit 0 vs stale Rust claim" path).
- F_new with low weight goes to quarantine, F_old stays active.
- Contested band produces both facts in `mind_search` results with
  `contested: true` flag.

**Gate to Step 2.3:** all three test paths work. Manual review of 5
real conflicts triggered against the migrated base — do they resolve
the way the author would have resolved them by hand?

### Step 2.3 — Loser dampening (0.5-1 day)

**Artifact:** when duel produces `flip`, F_old is not deleted — it
gets `valid_until = now`, `dampened: true`, and is excluded from
default `mind_search` ranking (still visible with explicit "include
dampened" filter).

**Tests:** dampened facts persist across restart, are not surfaced
in default search, are surfaced with explicit filter, can be
manually un-dampened via `mind_fact(action="restore", id=...)`.

**Gate to Phase 3:** dampened facts work across a full save/load
cycle. The whole duel mechanism is now end-to-end on real data.

---

## Phase 3 — doubt window + inheritance (3-4 days)

Goal: Mechanisms 2 and 3. These are the anti-ossification and
anti-echo guards.

### Step 3.1 — Inheritance flag (1 day)

**Artifact:** every fact returned by `mind_session_last`,
`mind_context`, or read from briefing is tagged
`inherited_unverified: true` in the active session's working set.

The flag is **silent by default** in user-facing output, per §11.5
positioning. It is voiced only when an inherited fact actively
conflicts with something stated in the live session.

**Internal effects:**
- inherited facts cannot co-confirm each other (the §5
  single-source rule)
- inherited facts enter duels with their `weight_new` discounted
- flag clears at the first live-session confirmation of the fact

**Tests:**
- session_last returns N facts → all loaded with flag set
- live session references one of them → flag clears for that one
- two inherited facts "agreeing" do not increment each other's
  confirmation count

**Gate to Step 3.2:** flag round-trips correctly across session
boundary (start → tool calls → end), is honored in confidence
calculations.

### Step 3.2 — Retrieval-triggered doubt window (1-2 days)

**Artifact:** when a high-entrenchment fact is surfaced by
`mind_search`, run a context-drift check:

```
drift_score = cosine_distance(
    fact.origin_context_vector,
    current_session_centroid(recent_memories)
)
```

If `drift_score > DOUBT_THRESHOLD` (starting at 0.4, calibrated in
Phase 4), the retrieval is logged as "surfaced but not confirmed"
and does **not** increment confirmation count.

After N (starting N=5) such non-confirmations, fact enters doubt
window: `confidence_score × 0.5` until next live confirmation.

**Tests:**
- known-stable fact retrieved in matching context → confirmation
  counts normally
- fact retrieved in clearly-drifted context (synthetic test:
  query about Python, fact about Rust) → no confirmation increment
- N=5 non-confirmations → fact's confidence visibly halved in
  subsequent retrievals

**Gate to Step 3.3:** the retrieval-triggered path works
end-to-end. Now the background path.

### Step 3.3 — Background active re-test (1 day)

**Artifact:** a tokio task running on adaptive cadence that walks
top-N entrenched-low-traffic facts and runs the drift check
**without** requiring a retrieval to trigger it.

Per §10 question 5:
- guard (a): per-process flag set during MCP call, background task
  yields if set
- guard (b): N capped at e.g. 50 per tick
- guard (c): cadence default 1 hour, doubles if no dependant
  changes since last tick (up to 24h cap), halves if many
  dependant changes (down to 5min floor)

**Tests:**
- task does not run during active MCP call (set flag, trigger
  task, verify no work done)
- pathological many-edits scenario → cadence drops, task still
  finishes per-tick within budget
- 24h idle → cadence at cap, task barely runs

**Gate to Phase 4:** the dual-budget cost model from §10 is real
code, not a comment in the spec.

---

## Phase 4 — calibration + bench (3-4 days)

Goal: tune the parameters set in Phases 2-3 against the real base,
then prove no regression on LongMemEval-S **and** report the
primary number on STALE (arxiv 2605.06527) — the field-recognised
benchmark for exactly the belief-revision behaviour this milestone
implements. The plan changed from "invent a custom QA harness" to
"run STALE" after the prior-art search; STALE makes the result
directly comparable to mem0=8.3, Zep=6.0, A-mem=5.1, and
CUPMem=68.0 on the same scale.

### Step 4.1 — Threshold sweep against LongMemEval-S (1-2 days)

**Artifact:** for each tunable constant
(`DUEL_FLIP_RATIO`, `DUEL_CONTESTED_RATIO`, `DOUBT_THRESHOLD`,
`SINGLE_SOURCE_DECAY_HALF_LIFE`), run R@k against LongMemEval-S
with `± 25%` and `± 50%` sweeps. Pick the value that produces:
- no regression in R@5 (must)
- highest precision on synthetic conflict-resolution test set
- fewest false-positive duels in author spot-check (10 random
  recent facts)

**Output:** `calibration_report.md` (local only — references real
facts; publishable summary commits to repo).

**Gate to Step 4.2:** report committed locally. Constants in code
match the chosen values, with comments linking to the report.

### Step 4.2 — LongMemEval-S regression bench (0.5 day)

**Artifact:** full LongMemEval-S 500q run on the v1.4-built system.
R@1 / R@5 / R@10 reported. Must be within ±1.0pp of v1.1 baseline
(the headline 98.2%) at minimum. This is the **regression check**
— it guards that v1.4 mechanisms did not degrade pure retrieval
recall. It is not the headline result for v1.4; that's Step 4.3.

If R@5 within tolerance: continue to Step 4.3.
If R@5 below tolerance: do not ship; calibration round 2 needed.

### Step 4.3 — STALE benchmark (1-2 days)

**Artifact:** clone STALE's published code+data (CC BY 4.0,
release confirmed in Appendix G of arxiv 2605.06527; verify URL
on first use), build the mgi-mind adapter (consume scenarios,
write to facts collection, query under the three behavioural
dimensions), run the full 400 scenarios / 1200 queries with the
Gemini-3.1-flash-lite judge.

**Budget:** order of magnitude tens to low hundreds of USD on a
flash-tier judge. Materially more on a frontier judge — not
needed for the headline number, but worth one frontier-judge run
on a 50-scenario subset to verify the cheap-judge result is not
artifacted by the cheap judge. Total estimated: $40-150 for the
full flash run, plus $20-50 for the frontier subset verification.

**Output:** STALE Overall %, broken down by metric (State
Resolution / Premise Resistance / Implicit Policy Adaptation)
and conflict type (Type I / Type II). Numbers commit to a new
`BENCHMARKS-STALE.md` section.

**Expected positioning of the result:**
- **≥ 30% Overall** (beats LightMem, materially beats mem0/Zep):
  publishable v1.4 release headline. The arXiv preprint angle
  becomes "first locally hosted open-source memory layer to ship
  the four mechanisms together, validated on STALE."
- **15-30%**: honest release, narrative is "first to ship; the
  full pipeline needs more calibration." Useful baseline for
  iteration.
- **< 15%** (worse than LightMem): do not publish the STALE
  number publicly until the next calibration round. Internal
  hold; debug; iterate. Releasing a bad number is worse than
  not releasing.
- **> 50%** (in CUPMem range): unlikely on the first pass;
  if it lands there, double-check the harness, then publish.

**Known limitation:** STALE is English-only. The Russian path
mgi-mind handles is not measured. Reported as a coverage gap in
the release notes, not papered over.

**Gate to release:** STALE result lands in one of the publishable
bands above and LongMemEval-S regression check passed.

---

## Phase 5 — release (0.5-1 day)

### Step 5.1 — Docs

- BENCHMARKS.md gets a new section: v1.4 R@k + the comparison to v1.1
- ROADMAP.md updated: v1.4 entry replaced with "shipped"
- CHANGELOG.md v1.4 entry written
- README.md updated only if the headline number moved by >0.5pp
- The synthesis document (`2026-06-04-FULL-SYNTHESIS-...`) and this
  plan are committed into the repo as `docs/design/v1.4-validity-
  model.md` and `docs/design/v1.4-implementation-plan.md` — design
  notes go in the repo when the work lands, not before

### Step 5.2 — Tag and release

`v1.4.0`. GitHub release with the changelog as notes, latest=true.

### Step 5.3 — arXiv preprint

The STALE result from Phase 4.3 is the headline. The preprint
frames the contribution as "first locally hosted, open-source
memory layer implementing the four mechanisms together, validated
on STALE" — citing the prior-art file (`docs/design/v1.4/prior-
art.md`), naming the implementation gap that the published memory
products leave open, and reporting the STALE result against their
baselines.

Not gated on Phase 4.3 number — the preprint goes out regardless
unless the number lands in the "do not publish publicly" band,
in which case the implementation note ships without the bench
number until the next calibration round.

---

## Effort summary

| Phase | Days | Critical gate |
|---|---|---|
| 0 — pre-work | 0.5 (one evening) | Cardinality registry, score-field skeleton |
| 1 — migration + measurement | 3-5 | Distributions printed, cardinality reviewed |
| 2 — duel rule | 3-5 | Spot-check ordering matches intuition |
| 3 — doubt + inheritance | 3-4 | Background guards work, both paths complete |
| 4 — calibration + bench (LongMemEval-S + STALE) | 3-4 | R@k regression + STALE in publishable band |
| 5 — release | 0.5-1 | v1.4.0 tagged |
| **Total** | **~13-20 focused days** | |

This is honest. Synthesis effort estimate said 3-6 weeks; with
weekends and breaks, ~12-19 focused days lands inside that range.
The split is intentionally lopsided toward Phase 1 (migration)
because that's the phase the synthesis flagged as the biggest risk
window, and the plan respects that.

---

## What this plan deliberately does not do

- **No formula written in advance.** Phases 2 and 3 calibrate
  against Phase 1 data. Writing formulas before that data is the
  shortest path to "compiled but doesn't work."
- **No judge-eval until after release.** $30-50 spent on bench
  before the mechanism is calibrated is wasted; spent after, it is
  measurement. Per §11.
- **No simultaneous work on backup, REST, decay, multi-tenant.**
  Those are v1.2 / v1.3 / v1.5 / v2.0. v1.4 is one mechanism, done
  cleanly, then shipped. Single-front discipline.
- **No marketing pumps mid-build.** No tweets, no Habr drafts, no
  Show HN preparation until Phase 5 docs are written. Pulling the
  narrative before the artifact is done is how projects miss their
  own deadline by writing about themselves.

## Privacy — what is public and what stays local

The Apache-2.0 license publishes the **code**, not the **data**.
Every step in this plan respects that line.

**What goes into the public mgi-mind repo:**
- Source code (the mechanisms, formulas, registry).
- `BENCHMARKS.md` numbers — but only from runs on the public
  `LongMemEval-S` synthetic corpus, never from runs on Mad's real
  base.
- `raw.json` under `benchmark/results/` — same constraint, public
  corpus only.
- Design notes, this plan, the synthesis document. These are
  methodology, not data.
- Synthetic examples in tests and docs (the "Mad uses Rust" worked
  example is a made-up illustration, not lifted from the actual
  store).

**What stays local on Mad's machine and is never committed:**
- The actual `~/mgimind/` data directory (the ~12k memories, all
  sessions, facts, provenance, vault, audit log).
- Phase 1 migration output that contains real memory content (the
  dependants walk produces per-fact lists; those lists stay local).
- Phase 2 spot-check selections of real facts ("known
  high-entrenchment 10 examples") — these are inspected locally to
  validate the formula, never committed.
- Phase 4 `calibration_report.md` if it cites real facts. The
  *numbers* (chosen thresholds, sweep results) can be summarized
  publicly with synthetic examples; the *real facts that drove the
  choice* stay local.

**Concrete commit-time rule:** before any `git add` in the mgi-mind
repo during Phases 1-4, check that the staged content references no
real memories. The migration scripts are designed to write their
detailed output to `~/Brain/migration-output/` (private, not in any
public repo), and only summary distributions (anonymous percentiles)
to anything that could end up public.

**What goes to OpenAI in Phase 5.3 (the comparative judge-eval):**
LongMemEval-S sessions and questions — *public corpus*. Mad's real
memories are not part of this run. The QA pipeline takes synthetic
test sessions, asks gpt-4o-judge to score answers, returns numbers.
Nothing from the local store crosses the network.

**One real risk that needs explicit discipline:** during Phase 2
formula development, the natural debugging move is "grep my real
base for examples that exercise this code path." That grep result
goes into the terminal, not into a commit. Mental rule: real
examples for thinking, synthetic examples for tests, anonymous
percentiles for public docs. If a paragraph in a public file
contains a memory Mad would recognize as his own, it does not ship.

---

## What unblocks tonight (the 2-hour window)

Phase 0 entirely. Both steps. 60-75 minutes if you don't rush, leaves
30-45 minutes of buffer for tests + commit. Tomorrow you wake up with
the substrate in place and can start Phase 1.1 (dependants backfill)
with a clear head.

If you want to keep going past Phase 0 tonight, the next thing in
line is Phase 1.1 step 1: write the `mgimind migrate dependants`
CLI scaffold (just the command, not the walk yet). That's another
30-45 minutes, lands you at the end of the 2-hour window with the
migration CLI ready to run tomorrow.

Do not start Phase 1.1 step 2 (the actual walk) tonight. That's a
real computation against the 12k base, runs maybe 10-30 minutes, and
its output (the distribution histogram) is the gate to Phase 2. You
want to look at that histogram with morning eyes, not 6am eyes.

---

## Status

This plan is the operational shadow of the synthesis. It does not
re-justify the design choices — those are in the synthesis. It does
not re-argue with critics — that's in the synthesis's §7 changelog.
It says: given the design as agreed, *do this in this order, with
these gates, with these honest budgets*.

The first thing this plan needs is a critic pass on the gates. Not on
the design — the design is settled. On the gates: are the right
artifacts named at each step, are the gates falsifiable, can you fail
a gate cleanly without breaking everything downstream. If a gate is
mushy, the plan will silently drift; if a gate is sharp, drift is
caught early.

But that's tomorrow. Tonight: Phase 0.
