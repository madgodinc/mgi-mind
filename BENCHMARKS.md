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

### Like-for-like vs other systems (planned)

To compare against a system that publishes QA accuracy (e.g. Mem0), run **their**
harness (`mem0ai/memory-benchmarks`) with the same answerer/judge model and top-k,
rather than comparing across metrics. Record the judge model, provider, and date
(LLM judges drift). This is a separate effort from the zero-API recall above.
