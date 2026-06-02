# Benchmarks

This file reports **what was actually measured**, with the command to reproduce it.
The one rule: report the metric you measured, and never put it next to a different
metric from another system.

## The metric: retrieval recall (R@k), zero-API

MGI-Mind has no generation layer — it is the memory, not the assistant. So its
native, honest number is **retrieval recall R@k**: given a question, does the gold
evidence appear in the top-k results of the hybrid search? This is measured with
**no LLM and no external API** — it runs entirely locally.

This is **not QA accuracy** (an LLM generates an answer and a judge-LLM scores it).
QA accuracy needs paid API calls and measures "memory + someone else's LLM", not the
memory itself. **Do not compare the R@k numbers here against another system's
LLM-judged QA numbers** — that is apples to oranges. A QA mode (answerer + judge, an
explicitly labeled API mode for like-for-like comparison with e.g. Mem0) is planned
as a separate, opt-in path; it is not part of this zero-API core.

## How recall is computed (LongMemEval, session-level)

For each question:
1. Its haystack sessions are ingested into an isolated, throwaway library — each
   session is one memory tagged with its session id.
2. `mind_search` runs the question against that library (hybrid dense + sparse).
3. The ranked results are collapsed to **distinct session ids**, in rank order.
4. R@k = a gold `answer_session_id` appears within the top-k distinct sessions.

Abstention questions (`_abs`, no in-haystack evidence) are **excluded** from the
recall denominator and reported separately — they test "say you don't know", not
retrieval. The retrieval config (model, dimension, reranking on/off) is printed in
the report header, because the number depends on it.

## Reproduce

Datasets are public, downloaded once — **no account or service is connected** for
this zero-API benchmark.

- **LongMemEval** — `xiaowu0162/LongMemEval` (HuggingFace). Start with the compact
  `longmemeval_s.json`.
- **LoCoMo** — Maharana 2024 (public). When added, report the standard **1540
  non-adversarial** subset (category 5 has documented ground-truth issues; do not
  rely on it blindly).

```sh
# Full run (long on CPU — embeds every haystack session of every question):
mgimind bench /path/to/longmemeval_s.json --output longmemeval_s.results.json

# Quick smoke (first N questions):
mgimind bench /path/to/longmemeval_s.json --limit 20
```

The run prints overall and per-question-type R@1 / R@5 / R@10 and writes raw
per-question results (gold vs retrieved, hit@k) to the `--output` file. Commit that
raw file alongside any number you publish, so the claim is checkable.

## Results

### LongMemEval-S — 2026-06-02, CPU

```
config: model=all-MiniLM-L6-v2 dim=384 rerank=false (sessions ranked by hybrid dense+sparse)
scored: 500 questions (0 abstention excluded)

Overall:
  R@1  = 85.2%
  R@5  = 98.2%
  R@10 = 99.4%

By question type:
  knowledge-update           n=78   R@1=94%  R@5=100% R@10=100%
  multi-session              n=133  R@1=86%  R@5=99%  R@10=100%
  single-session-assistant   n=56   R@1=100% R@5=100% R@10=100%
  single-session-preference  n=30   R@1=53%  R@5=93%  R@10=97%
  single-session-user        n=70   R@1=86%  R@5=99%  R@10=100%
  temporal-reasoning         n=133  R@1=80%  R@5=96%  R@10=98%
```

- **Date:** 2026-06-02
- **Build:** mgimind v0.8.1 (commit `a5fb6e4`, main)
- **Host:** Intel i5-12400F (6c/12t), 48GB RAM, CPU-only (no GPU acceleration)
- **Wall time:** ~1h 45min for 500 questions
- **Raw per-question JSON:**
  [`benchmark/results/2026-06-02-cpu-overnight/run01_minilm_rerank_off/raw.json`](benchmark/results/2026-06-02-cpu-overnight/run01_minilm_rerank_off/raw.json)
- **Full run notes:**
  [`benchmark/results/2026-06-02-cpu-overnight/README.md`](benchmark/results/2026-06-02-cpu-overnight/README.md)

Variance (multiple repeats) and additional configs (`rerank=on`,
`multilingual-e5-base`) will be added on subsequent minor releases rather than
in a single overnight burst. The plan is: every minor tag re-runs the headline
config above and publishes Δ; milestone releases run the full ablation matrix.

Do not paste a number you did not produce on this build — borrowing another
project's figure is exactly the overclaim this file exists to prevent.

## Counterfactual A/B — retrieval policy on / off

Companion benchmark to the LongMemEval recall numbers above. Measures the
**structural value of the search-before-answer policy**: take any
`mgimind bench` raw output, classify each question by the trigger table
(P1 must-search, P2 should-search, P0 no-search), and report the recall an
agent would NOT have if it didn't run retrieval at all.

This is not an LLM A/B (no generation, no judge). It quantifies the recall
ceiling the policy unlocks: ΔR@5 = with-policy R@5 − without-policy R@5.

### Protocol

```sh
mgimind bench-policy <raw.json from a prior `mgimind bench`>
```

Question-type → priority mapping (LongMemEval-S):

| Question type | Priority |
|---|---|
| single-session-user / preference / assistant, knowledge-update, multi-session | P1 must-search |
| temporal-reasoning | P2 should-search |
| _(none in LongMemEval-S)_ | P0 no-search |

### Results — 2026-06-02 (over the v0.8.1 baseline 500q run)

```
total questions: 500
  P1: 367
  P2: 133
  P0: 0

WITH policy:    R@5 = 98.2% (overall)
  P1 (n=367)    R@5 = 98.9%
  P2 (n=133)    R@5 = 96.2%

WITHOUT policy: R@5 =  0.0% (structural — no search → no retrieval hits)

ΔR@5 = +98.2 pct  ← recall unlocked by the policy
```

- **Raw policy JSON:** [`benchmark/results/2026-06-02-cpu-overnight/run01_minilm_rerank_off/policy.json`](benchmark/results/2026-06-02-cpu-overnight/run01_minilm_rerank_off/policy.json)

### Reading the number

- The "WITHOUT policy" R@5 = 0% is by construction: an agent that never
  searches doesn't see any candidate, so nothing can be in the top-5. The
  full Δ goes to "what would the policy save if the agent did skip search".
- LongMemEval-S contains no chit-chat / P0 questions (all 500 map to P1 or
  P2). The roadmap deliberately removed the P0 tier — false negatives cost
  more than false positives. The number you see is the **upper bound** of
  policy value on this dataset.
- A future dataset with explicit P0 questions ("hi", "thanks", "what time
  is it") would cleave the gap: the policy would *not* help there, but
  also wouldn't hurt — the trigger table says skip P0.
- **Not an LLM accuracy measure.** A real A/B with a generation step needs
  a like-for-like LLM-judged harness (see "Like-for-like vs other systems"
  below).

## Procedural memory — recall@k (phase Д6)

Independent benchmark from LongMemEval. Measures whether the procedural-memory
layer (`mind_learn` / `mind_recall`) surfaces the right playbook when an error
the agent has seen before comes back. Same zero-API rule: no LLM, no external
service, the answer is "is the gold fix in the top-k results".

### Protocol

Each dataset pair is `(error_signature, fix_description)` from a real fix
commit on a real OSS project. Run:

1. `mind_learn(error, fix, verified=false)` into an isolated bench library.
2. `mind_recall(error)` for the same error.
3. Gold position = first hit whose `fix` text matches the dataset pair.
4. R@k = fraction of pairs where the gold position is < k.

```sh
mgimind bench-procedural <dataset.jsonl> --output raw.json
```

Dataset format is JSONL with fields `{error, fix, language, stratum, id?, context?}`.

### Results — 2026-06-02 bootstrap (v0.14.0)

Mined locally with `scripts/scrape_procedural_dataset.py` from 10 OSS repos
(cargo, clap, commander.js, flask, pytest, qdrant, rust-clippy, rustlings,
tokio, yargs) at depth 5000 commits each. 111 pairs after filtering.

```
config: model=multilingual-e5-base dim=768 rerank=false
scored: 111 pairs

Overall:
  R@1  = 38.7%
  R@5  = 99.1%
  R@10 = 100.0%

By language:
  py    n=26   R@1= 50.0% R@5= 96.2% R@10=100.0%
  rust  n=75   R@1= 37.3% R@5=100.0% R@10=100.0%
  ts    n=10   R@1= 20.0% R@5=100.0% R@10=100.0%
```

- **Dataset:** [`benchmark/datasets/procedural-v010-bootstrap-111.jsonl`](benchmark/datasets/procedural-v010-bootstrap-111.jsonl)
- **Raw per-pair JSON:** [`benchmark/results/2026-06-02-procedural-bootstrap/raw.json`](benchmark/results/2026-06-02-procedural-bootstrap/raw.json)

### What the numbers say (and don't)

- **R@5 = 99.1%** is the headline. When the agent asks for a playbook the
  layer surfaces it in the top 5 nearly always.
- **R@1 = 38.7%** is realistic-and-low: many fix commits in the dataset share
  near-identical error signatures (e.g. two distinct CI flakes both saying
  "test failure on macOS"). With multiple plausible fixes for one signature,
  picking the *exact* gold at rank 1 is partly a coin flip — the metric to
  watch is R@5, not R@1.
- **TS R@1 = 20% (n=10)** reflects (a) small sample, (b) the multilingual-e5
  embedder is weaker on JS/TS than on Rust and Python in this corpus.
- The dataset is bootstrap-scale (111 pairs from 10 repos). The v0.10.0 target
  is 200+ pairs from 20+ repos with better stratum coverage (currently
  ~97% `runtime`; the scraper's last-resort symptom-sentence pattern catches
  too much as runtime). Replacing the heuristic with `git show --stat`-based
  file-type inference will rebalance.

### Like-for-like vs other systems (planned)

To compare against a system that publishes QA accuracy (e.g. Mem0), run **their**
harness (`mem0ai/memory-benchmarks`) with the same answerer/judge model and top-k,
rather than comparing across metrics. Record the judge model, provider, and date
(LLM judges drift). This is a separate effort from the zero-API recall above.
