# Prior art

This file lists the prior work that the v1.4 validity / relevance
model builds on. The synthesis is not a private discovery; the
mechanisms it implements were described in the field before this
project started. Citing them here is honest and load-bearing —
the contribution this project makes is the working open-source
implementation, not the ideas.

## Directly related

**STALE: Can LLM Agents Know When Their Memories Are No Longer
Valid?** Chao et al., May 2026.
[arxiv 2605.06527](https://arxiv.org/abs/2605.06527)

Benchmarks LLM agents on belief revision. 400 expert-validated
conflict scenarios, 1200 evaluation queries, contexts up to 150K
tokens, English. Defines two conflict types:

- **Type I (Co-referential).** Two observations of the same
  attribute with incompatible values, no explicit negation.
  Example: user states residence in Seattle; later mentions
  setting up utilities for a new apartment in Portland.
- **Type II (Propagated).** Update to one attribute cascades
  through logical dependency to invalidate a structurally
  distinct attribute. Example: a remark about a scorpion in a
  boot updates climate / pests and indirectly invalidates a
  prior city statement.

Three behavioural metrics: State Resolution (recognise the prior
belief is invalid), Premise Resistance (reject the false
presupposition), Implicit Policy Adaptation (proactively apply
the updated state). LLM-judge harness using Gemini-3.1-flash-lite
(95.8% human agreement). Published memory-layer scores on Overall:
**mem0 = 8.3, Zep = 6.0, A-mem = 5.1, LightMem = 17.8**. Frontier
LLM with raw context (Gemini-3.1-pro, no memory) = 55.2. The
paper's own architecture **CUPMem = 68.0**. Source release claimed
under CC BY 4.0.

What v1.4 takes from this:
- Phase 4 runs STALE as the primary release benchmark.
- The schema enum `EntryStatus` (Active / Contested / Stale /
  PropagationShadowed / Unknown) follows CUPMem's per-entry
  status pattern (KEEP / STALE / REPLACE / UNKNOWN) to express
  both Type I and Type II without a second flag.
- Type II propagated conflicts force the doubt-window mechanism
  in §3 to be more than a retrieval-triggered check; the active
  background re-test in §3 mechanism 2 was already there for
  ossification, and the same path handles propagation suspicion.

**Memory for Autonomous LLM Agents: Mechanisms, Evaluation, and
Emerging Frontiers.** Zhang et al., March 2026.
[arxiv 2603.07670](https://arxiv.org/abs/2603.07670)

Survey of the agent memory field. Formalises memory as a
write-manage-read pipeline. Taxonomy along temporal scope,
representational substrate, and control policy. Lists external
validation, uncertainty quantification with confidence decay,
adversarial probing, and expiration policies as future work.

What v1.4 takes from this:
- The four mechanisms in §3 (duel rule, doubt window,
  inheritance discount, bi-temporal axes) are exactly the items
  on this paper's future-work list. v1.4 implements that list.
- The map of the field guided the synthesis's prior-art search;
  if the survey lists something as solved, v1.4 doesn't claim
  novelty on it.

**SAVeR: Verify Before You Commit.** April 2026.
[arxiv 2604.08401](https://arxiv.org/abs/2604.08401)

Self-auditing of internal belief states before an action commits,
to prevent unsupported beliefs being stored and propagated.
Framework, not benchmark. Operates on the write path
(pre-commit), closer to a write-gate than to v1.4's runtime
doubt window. Adjacent to the inheritance-discount mechanism in
§3 but distinct in scope.

What v1.4 takes from this:
- The inheritance-discount mechanism (§3 mechanism 3) is in the
  same neighbourhood as SAVeR's pre-commit verification. Both
  address propagation of unverified beliefs. SAVeR audits before
  write; v1.4 discounts after read until in-session
  confirmation. Different timing, related insight.

## Explicitly not prior art (cleared up after closer reading)

**DReaMAD / "Belief Entrenchment in LLMs"** Mar 2026.
[arxiv 2503.16814](https://arxiv.org/abs/2503.16814)

This paper was initially cited as prior art on the "entrenchment"
mechanism. Full read showed it is about **multi-agent debate
dynamics**, not memory: entrenchment as biased static prior and
homogenized debate trajectories; the proposed DReaMAD architecture
adds prior elicitation and perspective diversity, reporting +9.5%
over ReAct on the MetaNIM Arena.

The word "entrenchment" collides terminologically with v1.4 §3
mechanism 1, but the mechanism is different. This paper does not
describe a memory-layer entrenchment ordering; it does not
overlap with v1.4 in any load-bearing way. Removed from prior art
in the v3→FINAL revision of the synthesis.

## Older foundations (still cited; predate the AI-agent era)

The synthesis §12 source pillars name the older foundations the
v1.4 mechanisms ultimately build on. These are not contested
prior art — they are the well-known field background:

- **Belief revision / epistemic entrenchment** (Gärdenfors et
  al., AGM-style work, ~1985).
- **Bi-temporal databases** (Snodgrass et al., TSQL2, ~1995; XTDB
  current).
- **Truth discovery / data fusion** (Yin, Han, Yu, ~2007).
- **Memory reconsolidation** (PubMed body of work, 2000s).

## How this list will be maintained

- New prior art that surfaces after this release is added with
  the source link, the section of the synthesis it touches, and
  what v1.4 takes from it.
- Items moved out of prior art (like DReaMAD) keep their entry
  here with the explanation, so the correction is traceable.
- This file ships in the same commit as any synthesis revision
  that depends on it; the synthesis's §7 changelog records the
  link.
