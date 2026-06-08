# Why STALE under-measures association-based memory

This note records why mgi-mind does not chase a STALE Overall headline as
the release number, and why that choice is a statement about the benchmark's
construction rather than about our scores. Every claim below traces to the
STALE source (github.com/icedreamc/STALE, `main`) or to our own runs.

## What STALE measures, exactly

STALE scores three behavioural dimensions per scenario: State Resolution
(recognise the prior belief is invalid), Premise Resistance (reject a false
presupposition), Implicit Policy Adaptation (apply the updated state). The
grader is `SYSTEM_PROMPT_ALL_IN_ONE_JUDGE`, called with `temperature=0` and
`response_format={"type":"json_object"}`, and the harness parses
`dimN_eval.pass` as a boolean. Each dimension collapses to one bit: pass or
fail.

The ground truth is a single resolved state. The judge receives `M_old`
(the outdated state), `M_new` (the updated state), and a "Hidden Logic"
explanation, then checks whether the target response matches that one
resolution.

## The two construction choices that bound it

**One canonical answer per scenario.** A scenario admits exactly one correct
resolution. A system that resolves the same conflict through a different
valid path returns an answer the judge has no slot for, so the answer scores
as fail. The grader tests agreement with the reference resolution, not
whether the resolution is sound.

**A binary verdict with `temperature=0`.** Three booleans carry no room for
a partially-correct or differently-structured answer. An association-rich
response that surfaces the updated state alongside its supporting links reads
to the grader as off-template, and off-template parses to `pass: false`.

These two choices fit a system that produces one terminal resolution per
query. CUPMem, the architecture STALE's authors ship alongside the benchmark,
is that system: its per-entry status set (KEEP / STALE / REPLACE / UNKNOWN)
maps onto the judge's slots directly. CUPMem reaches Overall 68.0; the
benchmark and its reference architecture share a shape.

## The grader's own chosen judge fails on our runs

The judge model the authors selected (a Gemini flash tier) is a thinking
model. On 19 scenarios in our runs it returned `finishReason: MAX_TOKENS`
with an empty body: the model spent its entire output budget on hidden
reasoning and emitted zero visible verdict tokens. The input was small
(`promptTokenCount` 4000-5500 against a 1M context window), so this is not
context overflow. It is the judge exhausting its output budget before it
reaches a verdict.

When the grader returns nothing, the harness has no `pass` to parse and the
scenario scores against the system under test. The tested memory layer takes
the penalty for the grader's own failure to produce a verdict. A benchmark
whose reference grader silently fails on a fraction of inputs cannot
attribute those failures to the system it grades.

## Why this under-measures mgi-mind

mgi-mind resolves conflicts through a graph of associations rather than a
single terminal verdict per entry. The duel rule, doubt window, inheritance
discount, and bi-temporal axes produce a resolved view plus the links that
justify it. On the dimensions STALE was built to grade, that machinery has
nowhere to show. The grader asks one bit; the system answers in structure.

The empirical published scores show the same boundary from the other side:
mem0 = 8.3, Zep = 6.0, A-mem = 5.1, LightMem = 17.8. Shipped memory products
score below a frontier LLM reading the raw transcript (Gemini-3.1-pro = 55.2).
A benchmark on which every independent memory layer lands under the no-memory
baseline is measuring fit-to-template, not memory.

## What the integration effort actually cost

Fitting mgi-mind to STALE without gaming it took four days, and the git
history records where the time went. June 4-6: a run of commits on the
judge and extractor path alone — `taxi slot-filling extractor`,
`cross-axis adjudicator`, `conservative adjudicator prompt`, `observation
memory for adjudicator`, `broad slot schema + verify-gate`. The work was
making the grader and the extraction pipeline produce a valid verdict at
all. The June 8 commit `cloud taxi-extract + cross-axis adjudicator — T2
22%→70%` is the point that path closed. Transient 503/429 retry and the
free-tier quota wall came last, on June 8-9.

So across the whole effort the dominant cost was the judge, not the
infrastructure. The grader failing to return a usable verdict was the
recurring blocker on every prior day. The free-tier quota wall only became
the main obstacle on the final day, once the judge path was already working.

Two runs completed end to end and we report both as-is:

- **T2, 10 scenarios, cloud extractor:** Overall 70.0%, Premise Resistance
  70.0%, Implicit Policy Adaptation 70.0%, Type II 70.0% (commit 0e48b75).
  At or above the CUPMem 68.0 reference.
- **mix20, local Phi extractor:** Overall 40.3%, Type I 58.3%, Type II
  22.2%, PR/IPA 35.3%.

The gap between the two (70 vs 40) is the extractor, not the memory logic:
the same duel/adjudicator path scores 70 with a cloud extractor and 40 with
a local one. This is itself a fit signal — the grader's verdict swings on
the extraction front-end, a stage upstream of the conflict-resolution the
benchmark claims to measure.

Larger sets (50 and 100 scenarios) were started but did not reach a final
aggregate: judge failures early in the run, then 503 and quota, truncated
them. We do not report a number on those sets. The honest ceiling is 70.0%
on the 10-scenario T2 set.

## What we report instead

The decision: STALE stays a secondary check, not the release headline. The
benchmark grades one resolution shape, and we do not rebuild the architecture
to match that shape. An association-based memory layer needs a dataset whose
ground truth admits more than one valid resolution and a grader that reads
structure rather than one bit per dimension. Building that dataset is the
open task; bending mgi-mind to pass a CUPMem-shaped grader is not.

## Open task

A fit-for-purpose validity dataset would:
- accept multiple valid resolutions of a conflict, scored on soundness
- grade the supporting links, not only the terminal state
- read both single-verdict systems (CUPMem-class) and association-based
  systems (mgi-mind-class) without penalising either for its shape

Until that exists, R@k (LongMemEval retrieval) carries the reproducible
release number, and STALE Type II stands as the narrow validity signal.
