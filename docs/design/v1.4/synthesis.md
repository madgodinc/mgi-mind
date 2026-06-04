# mgi-mind validity/relevance model — synthesis v3 (post critic round 2)

Date: 2026-06-04. Third revision after two critic rounds. Round 1
opened three holes in the v1 core that were defended-by-framing rather
than solved; v2 closed them structurally. Round 2 confirmed the
structural fixes but flagged two follow-throughs v2 had introduced
without finishing, plus a strategic-positioning question v2 left
ambient; v3 propagates the follow-throughs and claims the position
explicitly. This document is self-contained: a reviewer should be able
to read it end-to-end without the predecessor files. The change log
across all three versions is in **Section 7**.

---

## 1. Context

**Product.** mgi-mind is a local-first, single-Rust-binary, MCP-native
memory layer for AI assistants. v1.1 shipped on 2026-06-03. R@5 = 98.2%
on LongMemEval-S retrieval (zero-API, no LLM judge), on the default
install path (CPU MiniLM INT8 + reranker). Competitors: mem0 (57k
stars, $24M), Zep/Graphiti (32k combined), Letta (23k), Cognee,
supermemory. Their published numbers are end-to-end QA accuracy with a
gpt-4o judge; ours is pure retrieval recall — direct comparison
requires running mgi-mind through the same QA harness, which is a
later step.

**Problem.** Existing memory layers compute relevance as
`semantic_similarity × recency`. This is the naive rule. Two failure
modes are observable in production:

- **Fresh-overwrite drift.** User says "I switched to Go" once, the
  store overwrites the 8-month-entrenched "Mad writes Rust." A week
  later it can't explain why Mad's Rust code is still running.
- **Echo entrenchment.** A fact gets quoted across many sessions, each
  quote amplifies confidence. The store ends up *most* confident in
  facts it has confirmed *least* — they were re-quoted the most, not
  independently re-verified.

The competitor field doesn't have a clean answer to either. The
proposal below is structural, not a single feature.

---

## 2. The core in one paragraph

A contradicting fresh fact does not replace an old one — it **opens a
duel**. The old fact enters the duel with a handicap proportional to
its *entrenchment* (how many other memories depend on it × how often
it was retrieved and not contradicted). The fresh fact wins only by
accumulating *diverse* confirmations across distinct contexts, not by
being repeated by the same source. The loser is *dampened and
timestamped*, not deleted. Three further mechanisms keep the duel rule
from ossifying, from echoing, and from triggering on false conflicts.

The word "independent" from the previous synthesis has been replaced
with "diverse" throughout — this is not cosmetic, see Section 5.

---

## 3. Four mechanisms

### Mechanism 1 — Duel rule (conflict resolution)

A duel is triggered when `F_new` and `F_old` share `(subject,
predicate)` **and the predicate is single-valued** (see Section 4 for
why this matters). For multi-valued predicates, `F_new` is simply
added.

When a duel triggers:

1. Compute `entrenchment(F_old)` from:
   - **dependants** — count of other memories that semantically rely
     on it (already trackable via the knowledge-graph subsystem)
   - **confirmations** — retrievals where the fact was surfaced and
     not contradicted by the user, **weighted by source diversity**
     (Section 5)
   - **age-of-entrenchment** — time since first diverse confirmation,
     not since first write
2. Compute `weight(F_new)` from:
   - **inheritance discount** — was it surfaced by a live conversation
     turn, or carried in from memory (Section 6)
   - **diversity-weighted confirmations** — how many *distinct-context*
     observations support it (Section 5)
   - **bi-temporal stance** — valid_time vs transaction_time
     (Mechanism 4)
3. Resolve:
   - `weight(F_new) >> entrenchment(F_old)` → flip. `F_old` gets
     `valid_until = now`, `F_new` activated.
   - `weight(F_new) ~ entrenchment(F_old)` → **both stay as contested**.
     Retrieval surfaces both with a "contested" marker. Future
     observations break the tie.
   - `weight(F_new) << entrenchment(F_old)` → `F_new` enters the
     existing quarantine layer.
4. **The loser is never hard-deleted.** It keeps its trace for audit
   and for the case where the duel reverses later.

### Mechanism 2 — Doubt window (anti-ossification), with active re-test

The duel rule applied alone makes well-entrenched facts almost
unkillable. This is the same machinery as human stubbornness:
resistance to correction scales with age and weight, not with
correctness. So:

- **The more entrenched a fact, the more often it must re-justify
  itself.** Not less. This is counter-intuitive but matches the
  empirical neuroscience finding that retrieval without
  context-match does *not* strengthen the memory.
- On retrieval of a high-entrenchment fact: check whether the current
  session's context still matches the fact's context-of-origin. If
  the surroundings have drifted, the retrieval is marked as
  "surfaced but not confirmed" and does not raise entrenchment.
- After N retrievals-without-confirmation, the fact enters a doubt
  window: entrenchment weight is temporarily reduced; a
  moderately-weighted fresh contradiction can now win the duel.

**Active re-test (the v2 fix).** The doubt window described above is
triggered only by retrieval. That leaves a hole exactly where
ossification is worst: a fact that is *no longer being retrieved*
(low semantic match with current queries) never enters the doubt
window. It sits, entrenched, never re-tested.

To close this, the doubt-window mechanism includes a **scheduled
background pass**:

- Periodically walk the top-N entrenched facts that have low
  recent-access counts.
- For each, run a proactive context-drift check: compare the
  fact's origin-context vector to the centroid of recently-active
  memories.
- If drift exceeds a threshold, the fact enters the doubt window
  *without* needing a triggering retrieval.

This is not an optimisation. It is a structural requirement: without
it, Mechanism 2 leaves its primary failure mode uncovered. It also
upgrades the cost question (Section 9) from "cache entrenchment for
fast retrieval" to "cache *and* run scheduled re-tests of
low-traffic high-weight facts." Bigger budget; honest about it.

### Mechanism 3 — Inheritance discount (anti-echo, silent by default)

Every fact a session inherits from memory (briefing,
`mind_session_last`, files outside the live conversation) is loaded
with `inherited_unverified = true`. While the flag is set:

- The fact **cannot co-confirm another flagged fact.** Two flagged
  facts agreeing with each other is one source agreeing with itself.
- The fact's contribution to a duel is *discounted* until first
  in-session confirmation against live evidence.
- **The flag is silent by default.** Surfacing every inherited fact
  with "I have this from memory, not from this session" would train
  users to ignore the disclaimer. The flag is *voiced* only when an
  inherited fact is actively in conflict with something in the live
  session ("I have Rust in memory, you said Go — let me ask"). For
  everything else the flag works internally and silently. Trust in
  memory is built by being quietly correct, not by narrating
  provenance.

The flag clears at the first independent in-session confirmation.

### Mechanism 4 — Bi-temporal axes (anti-late-arrival)

Two clocks, not one:

- `valid_time` — when the fact became true in the world.
- `transaction_time` — when the system found out.

Optional third axis (`decision_time`) for audit: when did the system
decide to treat this as authoritative.

The naive "trust the latest write" rule loses on late-arriving
information: a May message saying "I moved in March" arrives later
than an April-written fact, but describes an earlier
real-world moment. Without separating the two axes there is no clean
resolution.

This is finance-database standard practice from ~1990 (Snodgrass,
TSQL2). Absent from almost every AI memory layer. In mgi-mind's
roadmap as v1.4; this proposal expands it to the full pair from the
single `valid_until` originally planned.

---

## 4. "Contradicts" — predicate cardinality

The duel rule above says it triggers on `(subject, predicate)` matches
"with single-valued predicate." This is load-bearing and was not in v1.

Without cardinality, the rule fires on every parallel co-existing fact:
"Mad uses Rust" and "Mad uses Go" become a duel even though a person
can write in two languages.

Spec:

- Every predicate-type carries a `cardinality` attribute:
  - `single` — at most one current `object` per `subject`
    (`primary_language`, `birth_year`, `current_project`)
  - `multi` — many `objects` allowed simultaneously
    (`uses_language`, `worked_at`, `speaks`)
  - `temporal-single` — single at any moment but historically a
    sequence (`primary_language` is actually this if you care about
    history — Rust in 2025, Go in 2026 — and it's the natural pair
    for bi-temporal axes)
- Cardinality is set when a predicate-type is first introduced —
  by the extractor, by `mind_fact_add(predicate, cardinality)`, or
  by an explicit type-registration call. It is *not* a hardcoded
  list.
- Default for unknown predicates is `multi`. Better to keep both
  facts than to start a false duel.
- Duel rule fires only on `single` or `temporal-single`. For `multi`,
  `F_new` is added alongside `F_old`.

This makes Mechanism 1 well-formed. Before this section was added,
the mechanism was specified at the verb level ("a duel") without
specifying which inputs are eligible.

---

## 5. "Independent" — the diversity model

The v1 synthesis leaned on the word "independent confirmation" without
specifying it. The critic correctly observed that in a single-user
mgi-mind install, *independence in the strict sense barely exists* —
every confirmation is from the same user. If the user is sincerely
mistaken and repeats the same wrong fact three times across sessions,
the v1 model counts three independent confirmations. By construction
the system would believe a wrong fact a single user happens to repeat.

The fix is to replace "independent" with **diversity**, defined
operationally and degrading gracefully:

1. **Source diversity** (highest weight). The fact is supported by
   provenance from distinct origin types — a live user assertion *and*
   a code-search snippet *and* a CI signal. The existing
   `mind_provenance_add` already records origin URL + tool used; this
   is the substrate.
2. **Context diversity.** Two confirmations from the same user in
   *different surrounding contexts* (token-overlap below a threshold,
   time gap above a threshold) count more than two confirmations in
   the same conversation. Counter-evidence in the literature: the
   A-B / A-C interference paradigm shows that contextual variability
   during retrieval is what produces robust learning, not raw
   repetition.
3. **External-signal weight** (strongest single confirmation type). A
   confirmation that comes from a deterministic external signal —
   `cargo test` exit 0, a commit landing, `mind_procedure_outcome(
   worked=true)` — weighs more than any number of conversational
   repetitions. This is already partially implemented for procedures;
   it generalises to all facts.
4. **Single-source decay.** Confirmations from the *same source*
   produce *diminishing returns*. Third confirmation from one user
   weighs less than the first; tenth weighs almost nothing. This
   prevents "I sincerely believed wrong, repeated three times" from
   passing as three independent confirmations. The decay curve is one
   of the open formula questions (Section 9).

**Net effect — and its follow-through.** In a single-user install with
no external signals, diversity weight degrades to "one source, with
diminishing returns" — the model doesn't pretend to have independence
it doesn't have. In a multi-tenant or tool-augmented install, real
diversity counts properly. The mechanism degrades to its weakest
sensible form rather than silently producing fake confidence.

That degradation has a follow-through the v2 of this document did not
spell out, and the critic round 2 caught: **in the chat-only
single-user default — which is mgi-mind's main use case — three of
the four diversity axes go quiet (source diversity ~ none,
external-signal weight ~ none, context diversity weak unless time
gaps are large). Single-source decay does the work alone, and "do the
work alone" reduces to "don't believe repeats too much."** That is
useful but it is *not* the strong confirmation signal the duel rule
was banking on.

Concretely this means the load-bearing signal in single-user defaults
shifts away from `confirmations` and toward `dependants`. Counting
how many other memories *structurally* depend on a fact is reliable in
single-user mode — the dependency graph is real, not echoed. Counting
how many times the same user said the same thing is, as the critic
correctly forced, almost decoratively weak.

This shift propagates into §6. The `confidence_score` multiplier
remains the slot, but its internal weighting between
`dependants` and `confirmations` is **install-mode-aware**:

- Single-user chat-only (the default): dependants carry most of the
  weight; confirmations decay fast and contribute only as a tiebreaker.
- Single-user with frequent external signals (tests/CI/code-grep):
  confirmations recover real weight through external-signal axis 3;
  dependants still carry more than `confirmations`-from-chat-alone.
- Multi-tenant: dependants and confirmations approach parity, because
  confirmations now carry real source-diversity.

This is not an optional tuning knob. It is a property of which
diversity axes have signal in a given install, and it must be reflected
in the confidence formula from day one. Pretending the formula is
mode-independent would walk us back into the v1 hole of overstating
independence.

---

## 6. How relevance is computed

Today (mgi-mind v1.1):

```
relevance(fact, query) = semantic_match × recency
```

Under the proposal:

```
relevance(fact, query, session) =
    semantic_match                  // existing hybrid dense+sparse
  × confidence_score                // see breakdown below
  × (1 - inheritance_discount)      // 1.0 if confirmed in this session
  × doubt_adjustment                // < 1.0 if in doubt window
                                    //   (retrieval-triggered OR
                                    //    background-recompute-triggered)
  × bi_temporal_validity            // 0 if outside valid_time, else 1.0
```

The `confidence_score` is the slot where the §5 follow-through lives.
Its internal shape is install-mode-aware, not a fixed combination:

```
confidence_score =
    w_dependants(mode) × normalised_dependants_weight(fact)
  + w_confirmations(mode) × diversity_weighted_confirmations(fact)
  + w_external(mode)     × external_signal_weight(fact)

where the mode weights are calibrated, not equal:

  single-user, chat-only (default):
      w_dependants    ≈ 0.7
      w_confirmations ≈ 0.1   (almost decorative, single-source decay
                               flattens repeats anyway)
      w_external      ≈ 0.2   (rare in this mode but high-quality when
                               present, e.g. one cargo-test signal)

  single-user with frequent external signals (dev with CI):
      w_dependants    ≈ 0.5
      w_confirmations ≈ 0.15
      w_external      ≈ 0.35  (this is where the mode actually earns
                               its confidence)

  multi-tenant:
      w_dependants    ≈ 0.4
      w_confirmations ≈ 0.4   (real source diversity finally lives here)
      w_external      ≈ 0.2
```

The numeric weights above are illustrative anchors, not the final
formula. The shape is what matters: in single-user chat mode the
system leans on the dependency graph, because that graph is real
even when the confirmation signal is mostly echo. In multi-tenant
the confirmation signal is recovered. In single-user-with-CI the
external-signal axis carries weight no chat repetition ever could.

Each multiplier blocks one failure mode. The product is a *calibrated
belief weight*, not a similarity score. `mind_search` returns ranked
beliefs with provenance and status, not just top-k hits.

---

## 7. What changed across critic rounds

This document went through two critic rounds. Round 1 found three holes
in the v1 core that were defended-by-framing instead of solved. v2
closed them structurally. Round 2 confirmed the structural fixes but
flagged two follow-throughs the v2 had not finished propagating, plus a
strategic question about positioning. v3 (this document) propagates
those follow-throughs and adds Section 11.5 for the positioning.

**Round 1 → resolved in v2:**

1. **"Independent" in a single-user install is almost always false.**
   Resolved by §5 (diversity model with single-source decay, external
   signals weighed highest, graceful degradation when diversity sources
   are absent).
2. **"Contradicts" was undefined; the duel would fire on parallel
   co-existing facts.** Resolved by §4 (predicate cardinality, `multi`
   as default for unknowns).
3. **Doubt window had a blind spot on the worst ossification cases.**
   Resolved by the active re-test pass in Mechanism 2 (scheduled
   background walk of high-entrenchment low-traffic facts).

Also dropped from v1:
- The "voice the inheritance flag on every load-bearing fact" rule
  (Mechanism 3 is silent by default; voiced only on active conflict).
- The framing of "synthesis-as-moat" in defensive claims (§11 is now
  honest about why this is first-mover advantage, not a structural
  moat).

**Round 2 → propagated in v3 (this revision):**

4. **Confidence weights had to shift from `confirmations` to
   `dependants` in single-user mode.** Round 2 observation: in the
   chat-only single-user default (the main use case), three of the
   four diversity axes from §5 go quiet — only single-source decay
   does the work, which is "don't trust repeats too much." That is
   weaker than the duel rule needs. Fixed by adding the
   install-mode-aware confidence breakdown to §6 (dependants ≈ 0.7
   in single-user chat-only, ≈ 0.4 in multi-tenant).
5. **The active re-test pass closed dyra 3 but moved the cost from
   correctness to latency without a budget.** Round 2 observation:
   the v2 declared the re-test "not an optimisation, a structural
   requirement" but left the cost-and-perf section as it was. Fixed
   by §10 question 5, which now splits the budget into two
   (retrieval-path hot, ms-scale, cached; background-path cold,
   scheduled, with three hard guarantees against contending with
   live tool calls).

Also added in v3:
- **§11.5 (positioning).** Round 2 made explicit what v2 had
  half-implied: the design produces calibrated-rather-than-maximised
  confidence, which is a real differentiator but a hard sell. v3
  claims the position deliberately (privacy-first, post-hallucination,
  agent-debugging audiences) instead of leaving it ambient.

**Round 4 → corrected in this revision:**

5. **Prior-art search uncovered three papers that pre-date the
   synthesis on the same axes.** STALE (arxiv 2605.06527, May
   2026) benchmarks belief revision in LLM agent memory with two
   formalised conflict types; SAVeR (arxiv 2604.08401) describes
   the echo mechanism on sampled trajectories; the March 2026
   survey (arxiv 2603.07670) lists the four mechanisms as future
   work. The "first to think of this" framing in §11 and §11.5
   was wrong; both sections were rewritten. The contribution is
   now claimed as "first locally hosted, open-source memory layer
   that implements the four mechanisms together and reports
   numbers on STALE," not as a private discovery. (A fourth paper
   the critic mentioned, DReaMAD / arxiv 2503.16814, turned out
   on full read to be about multi-agent debate, not memory; it is
   excluded from prior art with the apology that the v1 framing
   would have miscited it.)

6. **STALE's Type II propagated conflict broke the `conflict_
   pending: bool` flag in the schema.** Type II is when an update
   to one attribute cascades through logical dependency to make a
   structurally distinct attribute suspect. A boolean cannot
   express this. v3 of the synthesis assumed Type I-only
   conflicts; that assumption was tightened in this revision to
   require a small enum (`EntryStatus`) capable of distinguishing
   `Contested` (Type I, direct) from `PropagationShadowed`
   (Type II, indirect). The schema change landed in commit
   `20b80fc` on the same PR branch.

7. **Phase 4 bench changed from a custom QA harness to STALE.**
   STALE is the field-recognised benchmark for exactly this
   problem. Using it instead of inventing our own metric
   simplifies Phase 4 and produces a result the field knows how
   to read. Budget for the run revised upward: order of magnitude
   tens to low hundreds of USD on a flash-tier judge, not the
   $30-50 the v3 plan claimed.

8. **Language coverage gap acknowledged.** STALE is English-only.
   The Russian-language path mgi-mind handles will not be measured
   by this benchmark. Reported as a known limitation rather than
   papered over.

---

## 8. Worked example — Rust → Go

State at session start. Three facts about Mad's primary language:

| ID | Object | Confirmations | Dependants |
|---|---|---|---|
| F1 | Rust | 23 | 47 |
| F2 | TypeScript | 2 | 0 |
| F3 | Go (tried once) | 1 | 0 |

`primary_language` is `temporal-single`: one current value, history
preserved.

User says: *"I'm switching to Go, Rust got boring."*

**Competitor behaviour.** `UPDATE` F1 to "Go." Future answers proceed
from "Mad writes Go." A few days later when Mad mentions failing Rust
code, the store can't reconcile.

**Behaviour under the proposal.**

- `entrenchment(F1)` is high: 47 dependants × 23 confirmations across
  8 months, source-diversity moderate (user + code grep + repo refs).
- `weight(F_new)`: live source (inheritance discount = 0), but single
  confirmation from one source (no diversity yet), bi-temporal stance
  = "starting now," not retroactive.
- `weight(F_new) << entrenchment(F1)` → F_new enters quarantine as
  candidate. F1 stays active.
- The assistant surfaces a calibrated reply only on active conflict:
  *"Noted. Memory has eight months of Rust with 47 projects on it.
  When you write Go code in a future session I'll raise confidence and
  re-run the comparison."*

One-line branches:
- **A. Mad really switched.** Subsequent sessions with Go code, bench,
  goroutine discussion add diversity-weighted confirmations. F1 enters
  the doubt window (no Rust touched, drift detected by background
  re-test). Second-round duel: F_new wins, F1 → dampened, not deleted.
- **B. Passing thought.** F_new never re-surfaces. Quarantine
  single-source decay drops it below threshold within N days. F1
  continues normally.

The third v1 scenario (inherited echo) is folded into Section 5: a
fact arriving through `mind_session_last` instead of a live turn
carries the inheritance discount, cannot co-confirm itself through
further session summaries, and stays in quarantine until a live
in-session observation clears the flag.

---

## 9. What is already in the code (load-bearing only)

The strongest claim in this proposal is that we are **connecting
existing primitives**, not building from scratch. Three subsystems are
load-bearing:

- **Quarantine layer** (v0.11). Persists filtered entries with
  promote-on-repeat. This is the holding pen for losing fresh
  candidates in the duel rule.
- **Confirmation counter** via `mind_procedure_outcome`. Already
  separates "this was confirmed by an external signal" from "this was
  asserted by a user." Generalises directly to the external-signal
  weight described in Section 5.
- **Knowledge graph** with `(subject, predicate, object)` facts and
  `mind_fact_invalidate`. The substrate for the duel rule and for the
  cardinality registry described in Section 4. Bi-temporal extension
  is incremental.

Other subsystems matter (provenance, sessions, relevance gate,
audit log, vault) but they are not load-bearing for this proposal;
they make the implementation cleaner without changing the
effort estimate.

---

## 10. What is open — the actual work

Five formula / parameter / engineering decisions that depend on real
data, not on theory:

1. **Entrenchment formula.** Linear, logarithmic, recursive in
   dependants' own entrenchment? `confirmations` contribution shape?
   `age-of-entrenchment` decay vs accumulate?
2. **Diversity weighting formula.** How exactly is `source diversity`
   measured, how does `context diversity` combine with `single-source
   decay`, where is the threshold for "different enough"?
3. **Resolution thresholds.** Hard cutoffs or fuzzy bands between
   `flip`, `contested`, `quarantine`? Adaptive per predicate type?
4. **Doubt-window parameters.** N retrievals-without-confirmation
   before window opens. Frequency of the *active* re-test background
   pass. Drift threshold for the centroid comparison.
5. **Cost / performance model — now two budgets, not one.** The v2
   synthesis closed dyra 3 (doubt-window blind spot) by adding a
   scheduled background re-test. The critic round 2 correctly pointed
   out that the re-test was added to the design but not to the
   budget. Closing this honestly means the latency picture splits in
   two:

   - **Retrieval-path budget (hot).** Stays at the ms-scale lookup
     mgi-mind ships with. The `confidence_score` from §6 must be
     cached per fact; cache invalidation fires when a dependant of
     the fact is added, removed, or itself changes confidence enough
     to cross a threshold. Entrenchment is **not recomputed on the
     retrieval path** — it is read from cache. This is the contract
     with the warm-process narrative we sell publicly.

   - **Background-pass budget (cold, scheduled).** A separate idle-time
     loop with three hard guarantees:
       (a) **Never runs while an MCP tool call is in flight.** A
           simple per-process flag set on enter, cleared on exit.
           This protects the latency contract from background
           contention.
       (b) **Caps its per-tick scan** — top-N entrenched-low-traffic
           facts, where N is small enough that one tick fits inside
           the longest expected idle window. The walk is amortised
           across many ticks, not done in one breath.
       (c) **Adaptive cadence.** Default tick interval slow (hourly,
           initially); rate increases when many dependant graph
           changes have occurred since the last pass, decreases when
           the graph is quiet. The signal is "how stale could the
           cache plausibly be?" — not a hardcoded clock.

   - **Where the cache lives.** Qdrant payload for the per-fact
     `confidence_score` and last-recompute timestamp; a sidecar
     dependant-index for the invalidation triggers (Qdrant doesn't
     express graph edges natively).

   - **Net consequence for the public narrative.** "Single warm
     process, ms lookup" remains true on the retrieval path. We
     additionally have "low-priority background process that
     reconciles staleness, scheduled to never collide with active
     queries." This is honest — and it explains why we don't ship
     "memory that costs you nothing": it costs an idle budget. That
     idle budget is the price of not ossifying.

6. **Install-mode profile selection.** §6 now ships three profiles
   (chat-only / dev-with-CI / multi-tenant) with different weight
   distributions. Open: who switches the profile, and when?

   - **Manual via config** is the safe default — predictable, no
     surprise weight shifts. Cost: the user has to know which profile
     they are in, and re-pick after a real change in their workflow.
   - **Auto-detect by signal density** is the tempting alternative —
     "we saw N external-signal confirmations in the last K days,
     promote to dev-with-CI." Cost: the auto-switch can pull weights
     out from under an active session. A fact that was confidence-
     ranked 0.8 in chat-only might recompute to 0.6 in dev-with-CI
     mid-conversation, with no user-visible cause. This is the same
     class of bug as the doubt-window's retrieval-only blind spot
     before v2 — a mechanism that helps overall but creates a small,
     wrong-feeling local discontinuity.

   The honest answer is probably **manual config with an auto-detected
   recommendation** — the system notices "your signal mix suggests
   dev-with-CI, here is what would change" and surfaces it, but does
   not switch on its own. That keeps the user in control of the
   confidence calibration that the system reports back to them.
   Calibration of someone else's beliefs without consent is exactly
   the trust failure we are trying to avoid.

7. **Cold-start for the install-mode profile.** A fresh install has
   no dependant history, no confirmation history, no external-signal
   history. Which profile does it start on? Three options:

   - **chat-only by default**, promote to dev-with-CI when external
     signals accumulate. Safe but slow — a new install on a CI-heavy
     workflow under-reports confidence for weeks before the profile
     catches up.
   - **dev-with-CI by default**, demote if external signals never
     appear. Wrong-direction risk: the system claims confidence it
     has not yet earned, then quietly demotes — the inverse of what
     we want.
   - **A neutral cold-start profile** — equal-ish weights, transitions
     into one of the three steady profiles as signal accumulates.
     Most honest, most plumbing.

   v2 closed cold-start for the duel rule (degrades to plain recency
   while history accumulates). The install-mode mechanism added in v3
   created its own cold-start that the v2 fix does not cover. Likely
   answer: the neutral cold-start profile, with the user able to
   pre-declare their mode at `mgimind init` time as a hint. Decided in
   week 2 of the schedule, not before, because the migration of the
   existing 12k base settles part of this question anyway (the author
   install is not cold-start — it has years of history).

These seven are the work. Each has a non-obvious answer. Each will
require iteration against the actual ~12k-memory base in mgi-mind's
author install.

---

## 11. Effort and defense — honestly

**Effort.** 3-6 focused weeks, by the author's own count:

- Week 1 — formulas in Rust, with unit tests against synthetic
  conflict scenarios. Cardinality registry. Diversity-weighted
  confirmation counter.
- Week 2 — schema migration over the existing ~12k-memory base
  (backfill entrenchment, confirmations, inheritance flags). This
  week is a real risk: old memories have no confirmation history,
  no provenance for many of them, and the migration choices set
  the calibration baseline for everything that follows.
- Week 3 — retrieval pipeline integration; entrenchment cache;
  background active re-test pass; smoke bench against the
  existing LongMemEval-S R@k to confirm no regression.
- Weeks 4-6 — edge cases, behavioural patterns under real use,
  iteration on formulas. The bulk of the work lives here.

Then — and only then — a comparative QA bench against
mem0 / Zep / supermemory on LongMemEval-S with gpt-4o judging.
$30-50. This is the *measurement* that closes the loop, not the
*marketing* hook. If QA accuracy moved, the mechanism works. If it
didn't, the formulas need another iteration before publication.

**Defense, honestly.** The v1 synthesis claimed a "structural moat"
out of the four-discipline reading. A later prior-art search
(critic round 4) showed even that reframing was over-confident: the
four mechanisms are not novel. STALE (May 2026) benchmarks exactly
this problem; arxiv 2503.16814 uses the word "entrenchment" on LLM
reasoning and documents the same ossification mode; SAVER
(2604.08401) describes the echo mechanism; the March 2026 survey
(arxiv 2603.07670) lists external validation, uncertainty
quantification with decay, adversarial probing, and expiration
policies as open future work. The mechanisms in this synthesis are
the field's open agenda, not a private discovery.

What remains real:

- **Layer 1 (timestamp) is a record, not a moat.** The
  `~/Brain/ideas/` private commits (`6fef735` → `4edaf52`,
  2026-06-04) are the audit trail of how the synthesis took its
  current shape, including the prior-art correction. They do not
  establish priority over the four cited papers; they establish
  authorship of *this implementation* of mechanisms the field
  already named.
- **Layer 2 (Apache-2.0 working implementation) is the actual
  contribution.** The cited papers describe; mem0 / Zep /
  supermemory ship products without these mechanisms; nothing in
  the prior-art search shows a working, local, open-source memory
  layer that implements the four mechanisms together and reports
  numbers on a recognised benchmark. That gap — between described
  and shipped — is the lane. It is narrower than "I invented
  this" but it exists, and it is testable: a working
  implementation either lands STALE numbers above the published
  baselines or it does not.
- **Layer 3 is speed, and now finite.** The four-discipline reading
  is why this implementation can ship soon. It is not why nobody
  else will ship something similar later. With STALE published and
  the survey openly listing the agenda, the window is shorter than
  the v1 synthesis assumed. Aim for the speed advantage to convert
  into a shipped, used, cited project before the window closes.

No patent path was ever in the active plan; the prior-art search
makes the question moot anyway.

---

## 11.5. Positioning — "described, then shipped"

This section was forced through three critic rounds (calibration
round 2; prior-art round 4). It claims the positioning explicitly
because positioning is downstream of the core design and the design
produces a specific shape of contribution that has to be named
deliberately.

The honest situation, after the prior-art search.

The four mechanisms in §3 are not novel. STALE (arxiv 2605.06527,
May 2026) benchmarks exactly the failure modes the synthesis names
and distinguishes the two conflict types (direct, propagated).
SAVeR (arxiv 2604.08401) describes the echo mechanism on sampled
trajectories. The March 2026 survey (arxiv 2603.07670) lists
external validation, uncertainty quantification with decay,
adversarial probing, and expiration policies as the field's open
agenda. The mechanisms in this synthesis are that agenda, not a
private discovery.

The empirical state of the published memory layers on STALE:
mem0 = 8.3%, Zep = 6.0%, A-mem = 5.1%, LightMem = 17.8%. The best
frontier LLM with no memory layer at all (Gemini-3.1-pro reading
the raw transcript) reaches 55.2%. CUPMem, the architecture STALE's
authors propose alongside the benchmark, reaches 68.0%. The
*published memory products fail this benchmark catastrophically* —
worse than throwing the dialogue at the model. That gap is not
rhetorical. It is a number anyone can verify.

The contribution this synthesis makes is therefore narrower than v1
claimed and sharper than v3 claimed. It is not "I invented this."
It is: **first locally hosted, open-source memory layer that
implements the four mechanisms together, in a single Rust binary,
and reports numbers on STALE**. The field described the mechanisms;
the published memory products did not ship them; we are between the
two and the gap is testable.

This reframes the audience and the channels.

- **The audience that reads STALE.** Researchers and engineers who
  already know about the belief-revision problem in agent memory.
  For them, the prior-art citation is the credential — it shows we
  read the field. The contribution is the working implementation
  and the STALE numbers. arXiv preprint, r/LocalLLaMA, lobste.rs,
  HN with the STALE result as the headline.

- **The audience that has been burned by hallucinating memory.**
  Privacy-first engineers and teams building agents where silently
  over-confident memory is a debugging nightmare. For them the
  pitch is "the published memory products score in single digits on
  this benchmark; we score [X]; here is why; here is the binary,
  one curl." This audience cares about behaviour, not about who
  invented entrenchment ordering.

- **The audience that does not get it.** Mass-market developers
  shopping memory-as-a-service. They read "first to implement what
  STALE measures" as jargon. They are not our customer; spend
  acquisition effort elsewhere.

Narrative discipline that follows from this:

1. **Do not pitch on "I invented this."** STALE, SAVeR, and the
   survey are one search away. Anyone qualified will find them and
   the pitch will read as carelessness about prior art. Lead with
   the prior-art citation, then the implementation gap.
2. **Pitch on "first to ship and measure."** The shape is "the
   field described the mechanisms; the published products score
   8.3 / 6.0 / 5.1 on the benchmark that measures them; this is
   the first local open-source implementation that puts the
   mechanisms together and reports [X] on the same benchmark."
   Defensible, falsifiable, and respects the work the field has
   already done.
3. **Educate by leading with the failure case.** Show the
   Seattle→Portland example. Show a mem0 install confidently
   believing the wrong city. Then claim the alternative. The
   prior-art papers do this groundwork for us; we cite, we don't
   re-derive.

Channels worth time:

- arXiv preprint with STALE result, citing the four prior-art
  papers, framing the contribution as the implementation gap.
- r/LocalLLaMA, lobste.rs, privacy-tooling newsletters: yes,
  these audiences read this framing as a feature.
- Show HN: only when the STALE number is in hand. The headline
  fight is on STALE, not on QA accuracy of LongMemEval-S (where
  mem0's published numbers come from a benchmark that, per
  STALE's own analysis, does not measure belief revision).
- Mass-market dev channels and listicles: no.

The speed of the field matters more than v3 assumed. STALE was
published in May 2026; the survey listing the agenda is March 2026.
Whoever ships first on STALE owns the lane. Whoever ships in
quarter 4 instead of quarter 3 may find that someone else already
has, because the agenda is public. The contribution depreciates
with time in a way the v3 framing did not acknowledge. Move
accordingly.

---

## 12. Source pillars

For the reviewer to push back on or extend:

1. **Belief revision / epistemic entrenchment.** Gärdenfors et al. on
   AGM-style belief revision (~1985). The naive
   conservative-revision-with-fresh-priority rule was shown
   internally inconsistent; the field's answer was entrenchment
   ordering.
2. **Bi-temporal databases.** Snodgrass et al., TSQL2 (~1995); XTDB
   (current). Two-clock model is standard in regulatory / financial
   data; absent from almost every AI memory layer.
3. **Truth discovery / data fusion.** Yin, Han, Yu (~2007) on
   multi-source agreement and source dependency. The counter-
   intuitive result: agreement on an *error* is a stronger signal
   of source dependency than agreement on truth. The
   diversity-and-single-source-decay reframe in Section 5 is the
   right way to import this finding into a memory layer that mostly
   has a single user.
4. **Memory reconsolidation.** PubMed body of work on
   retrieval-induced facilitation, A-B / A-C interference paradigm,
   reconsolidation resistance scaling with age. The empirical
   finding that recall without context-match does *not* strengthen
   the memory is direct precedent for both the retrieval-side and
   the background-side of Mechanism 2.

---

## 13. Where to push next

Pre-empted easy critiques:

- *"This is just bi-temporal with extra steps."* Bi-temporal handles
  Mechanism 4 only; Mechanisms 1, 2, 3 are not in the bi-temporal
  literature.
- *"This is just Bayesian belief update."* Vanilla Bayes assumes
  i.i.d. observations. Mechanism 3 (inheritance discount) and Section
  5 (single-source decay) are precisely the dependency-aware
  corrections vanilla Bayes drops.
- *"Why won't mem0 just add this?"* They might. The synthesis is
  public-domain pieces; what is harder to copy is the *integration
  decisions* (where the thresholds sit, how caching works, how the
  migration goes). The defense is first-mover with a working
  implementation, not a structural moat — see Section 11.

Where push-back is genuinely wanted (after two rounds, these are the
ones still standing):

- **Cardinality bootstrapping for ~12k existing memories.** Almost no
  fact has a registered cardinality. The migration has to choose
  defaults. `multi` for everything is safe but kills the duel rule
  for the existing base — and the existing base *is* what gives the
  system any entrenchment at launch. Is there a heuristic to infer
  cardinality from the existing data (e.g. "this predicate has only
  ever been used with one distinct object per subject" → propose
  `single`, prompt for confirmation)? Without a good answer, week 2
  of the schedule (schema migration) is the place this hurts.

- **Single-source decay curve.** Now load-bearing in the §6
  install-mode-aware confidence formula. The decay determines how
  strongly the system resists a sincerely-mistaken user — too
  aggressive and the user cannot update memory at all; too lenient
  and three repeats pass as confirmation. May not have a good
  universal answer; may need to be per-predicate (a `current_project`
  fact should decay confirmations slowly because projects do not
  change daily; a `mood` fact should decay them fast). Push on
  whether per-predicate calibration is a slippery slope into
  hand-tuned configs, or a clean axis.

- **Active re-test's adaptive cadence.** §10 question 5 says the
  background pass adjusts cadence by "how stale could the cache
  plausibly be?" This phrase is doing a lot of work. Concretely, what
  signal triggers a cadence increase — dependant-graph edit count
  since last pass? Average confidence-cache age? Both? Where is the
  saturation cap that prevents a pathological case (many small edits)
  from starving the retrieval path?

- **The "audience that gets it" might be too narrow to sustain
  attention.** §11.5 claims privacy-first / agent-debugging / post-
  hallucination audiences as the right buyers. That intersection is
  real but small. Is the positioning sustainable on its own, or does
  it need a wedge into an adjacent broader audience (e.g. "memory
  for self-hosted RAG", "memory for compliance-bound deployments")?
  Push on whether the calibrated-confidence story can carry the
  project to enough traction to fund the next thing, or whether it
  is a perfect-but-quiet niche.

- **Layer composability.** The four mechanisms compose; we have shown
  they don't structurally fight each other. But under what conditions
  do they *reinforce* each other into a pathological state? E.g. a
  fact with high inheritance-discount + low diversity + entering doubt
  window simultaneously — what does the formula say, and is that the
  right answer? The composition has not been pressure-tested in any
  worked corner case.

These are the questions a senior systems person should ask after the
v3 revision. They are not bikeshedding; the core has been pushed
twice now, and each push has produced a real structural change. A
third push that lands on any of the above will also produce one.

---

## 14. Status

- **Theory.** Tightened after two critic rounds. Round 1 closed three
  pre-existing core holes structurally (§4 cardinality, §5 diversity
  model, active re-test in Mechanism 2). Round 2 propagated two
  follow-throughs that round 1 introduced but did not finish (the
  install-mode-aware confidence weights in §6, the two-budget cost
  model in §10) and added §11.5 to claim positioning explicitly.
- **Primitives.** Three load-bearing subsystems already in code;
  others helpful but not required.
- **Effort.** 3-6 focused weeks; week 2 (schema migration, now
  including cardinality bootstrapping) is the real risk window.
- **Defense.** Timestamp + Apache-2.0 first-mover implementation.
  No patent ambition yet — nothing concrete to claim until the
  formulas crystallise.
- **Positioning.** Calibrated, not maximised — explicit choice in
  §11.5, with audience and channel implications.
- **Next.** §10 (the five formula / parameter / cost decisions) and
  §13 (the five questions still standing after two rounds) mark the
  remaining work.

Two critic rounds produced two structural revisions, each catching
something real. Round 2 closed its own loop: after the v3 fixes
landed, the same critic explicitly stepped off the ball with
"критиковать его за то, что он ещё не написал формулу, которую
невозможно написать без прогона на 12k базе — было бы bikeshedding"
and added two further open-work items (§10 questions 6 and 7) before
stopping. That is the calibrated stop the v1 framing was meant to
prevent and could not.

The theory is not "sound" in the sense of "above further criticism";
it is sound in the sense of "the holes named so far have been closed,
the document now names where the next holes are most likely, and an
adversarial reviewer running the same loop a second time concluded
that further pre-implementation criticism would generate more heat
than light." Those are different claims. v1 conflated them. v3 does
not, and the second critic round confirmed that distinction.

Next step is not another revision of this document. Next step is week
1 of §11 — the formulas land in Rust, against synthetic conflict
scenarios, with the 12k-memory base as the post-migration target.
This document goes into the repo as a design note when v1.4 work
begins, edited only if implementation forces a change to the spec.
