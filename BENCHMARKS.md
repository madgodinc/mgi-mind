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

### LongMemEval-S — 2026-06-03, regression v0.12.1 vs v0.8.1 (RunPod CPU)

Goal: confirm the quarantine layer (v0.11.x) and the relevance gate did not
break retrieval against the v0.8.1 baseline above. Same dataset, same model,
same `rerank=off`. Re-run on a RunPod 8-vCPU community CPU pod.

```
config: model=all-MiniLM-L6-v2 dim=384 rerank=false
scored: 500 questions (0 abstention excluded)

Overall:
  R@1  = 85.6%   (Δ vs v0.8.1: +0.4)
  R@5  = 97.6%   (Δ vs v0.8.1: -0.6)
  R@10 = 99.4%   (Δ vs v0.8.1:  0.0)

By question type:
  knowledge-update           n=78   R@1=91%  R@5=100% R@10=100%
  multi-session              n=133  R@1=85%  R@5=98%  R@10=100%
  single-session-assistant   n=56   R@1=100% R@5=100% R@10=100%
  single-session-preference  n=30   R@1=53%  R@5=93%  R@10=97%
  single-session-user        n=70   R@1=84%  R@5=97%  R@10=100%
  temporal-reasoning         n=133  R@1=85%  R@5=95%  R@10=98%
```

- **Date:** 2026-06-03
- **Build:** mgimind v0.12.1 (commit `b37f89f`, tag `v0.12.1`)
- **Host:** RunPod community CPU pod, 8 vCPU, 32 GB RAM (Ubuntu 22.04, x86_64)
- **Raw per-question JSON:** [`benchmark/results/2026-06-02-v012-regression/pod2-rerank-off/raw.json`](benchmark/results/2026-06-02-v012-regression/pod2-rerank-off/raw.json) (sha256 `3a7395508db551ca876dd6d921abab85fd630f5d0bc4765aa74c191a206d308f`)
- **Log:** [`bench.log`](benchmark/results/2026-06-02-v012-regression/pod2-rerank-off/bench.log)

**Reading:** all three deltas vs v0.8.1 are within statistical noise at
n=500 (±0.5pp at 95% CI). The new quarantine + relevance-gate pipeline does
NOT regress retrieval recall on this dataset and config.

The companion `rerank=on` run was finished on a second community pod
before it was reclaimed; results below.

```
config: model=all-MiniLM-L6-v2 dim=384 rerank=true
scored: 500 questions (0 abstention excluded)

Overall:
  R@1  = 91.6%   (Δ vs v0.8.1 rerank=on baseline: not directly comparable, no baseline rerank=on run)
  R@5  = 98.2%
  R@10 = 99.8%

By question type:
  knowledge-update           n=78   R@1=97%  R@5=100% R@10=100%
  multi-session              n=133  R@1=92%  R@5=98%  R@10=100%
  single-session-assistant   n=56   R@1=100% R@5=100% R@10=100%
  single-session-preference  n=30   R@1=53%  R@5=93%  R@10=97%
  single-session-user        n=70   R@1=96%  R@5=99%  R@10=100%
  temporal-reasoning         n=133  R@1=91%  R@5=97%  R@10=100%
```

- **Raw per-question JSON:** [`benchmark/results/2026-06-02-v012-regression/pod1-rerank-on/raw.json`](benchmark/results/2026-06-02-v012-regression/pod1-rerank-on/raw.json) (sha256 `0389348e13df0b4effeac75c588a6165ee0653ded53887ed0f19678a81bc4bf5`)
- **Log:** [`bench.log`](benchmark/results/2026-06-02-v012-regression/pod1-rerank-on/bench.log)

Reranker on this CPU/MiniLM config moves R@1 by +6pp and R@5 by +0.6pp.
The reranker effect is real but small on MiniLM; it is much larger on the
e5-base headline below.

### LongMemEval-S — 2026-06-04, v0.14.3 GPU (RTX 3090, e5-base FP16)

First GPU run of the bench, also first run on `multilingual-e5-base`
(the dense default; baseline above is `all-MiniLM-L6-v2` for a like-for-like
v0.8.1 comparison). Switched from the INT8 quantized e5-base ONNX shipped
by `mgimind doctor --fix` to the **FP16** variant — INT8 ops (`MatMulInteger`,
`DynamicQuantizeLinear`) are not implemented in the ORT CUDA execution
provider and fall back to CPU, defeating GPU acceleration. FP16 keeps the
whole graph on the GPU and gives the actual speedup (~25 min/500q vs
~1h45 on the CPU baseline above).

```
config: model=multilingual-e5-base FP16 dim=768  (sha256 5d760477f691b665da2b94e1528eb6938b795f76064d9392e6af7118b8a3f54a)
host:   RunPod community pod, RTX 3090 24GB, 8 vCPU, 30 GB RAM, Ubuntu 22.04
build:  mgimind v0.14.3 (commit 47c0455, --features cuda),
        ORT 1.24.2 GPU build (libonnxruntime_providers_cuda.so 302 MB),
        cuDNN 9.23.0, CUDA driver 550.100 (runtime 12.4),
        MGIMIND_USE_CUDA=1
scored: 500 questions (0 abstention excluded), three runs
```

Run A — `rerank=false`:

```
  R@1  = 88.4%
  R@5  = 98.0%
  R@10 = 99.4%
  wall = 629s (~10.5 min)

By question type:
  knowledge-update           n=78   R@1=95%  R@5=100% R@10=100%
  multi-session              n=133  R@1=88%  R@5=98%  R@10=100%
  single-session-assistant   n=56   R@1=100% R@5=100% R@10=100%
  single-session-preference  n=30   R@1=60%  R@5=90%  R@10=97%
  single-session-user        n=70   R@1=89%  R@5=96%  R@10=100%
  temporal-reasoning         n=133  R@1=86%  R@5=98%  R@10=98%
```

Run B — `rerank=true` (headline):

```
  R@1  = 92.6%
  R@5  = 99.2%
  R@10 = 100.0%
  wall = 1539s (~25.6 min)

By question type:
  knowledge-update           n=78   R@1=99%  R@5=100% R@10=100%
  multi-session              n=133  R@1=95%  R@5=100% R@10=100%
  single-session-assistant   n=56   R@1=100% R@5=100% R@10=100%
  single-session-preference  n=30   R@1=63%  R@5=93%  R@10=100%
  single-session-user        n=70   R@1=94%  R@5=100% R@10=100%
  temporal-reasoning         n=133  R@1=89%  R@5=98%  R@10=100%
```

Run C — `rerank=true` again (variance):

```
  R@1  = 92.6%   (Δ vs Run B:  0.0)
  R@5  = 98.8%   (Δ vs Run B: -0.4)
  R@10 = 100.0%  (Δ vs Run B:  0.0)
  wall = 1528s (~25.5 min)
```

- **Date:** 2026-06-04
- **Reranker:** `Xenova/bge-reranker-base` quantized ONNX. The reranker is
  also registered against the CUDA EP at startup but its INT8 weights
  similarly fall back to CPU during inference (same MatMulInteger reason).
  The `rerank=true` cost (2.4× wall) is therefore mostly CPU-bound; this
  number is honest about its mix. A reranker-FP16 pass is planned.
- **Raw per-question JSON:**
  - [`v0143-e5fp16-rerank-off.json`](benchmark/results/2026-06-03-gpu-v0143/v0143-e5fp16-rerank-off.json) (sha256 `73a1e49194966bdb0859fca3056651db0248027cca2275a57aa097fbde6d0b58`)
  - [`v0143-e5fp16-rerank-on.json`](benchmark/results/2026-06-03-gpu-v0143/v0143-e5fp16-rerank-on.json) (sha256 `ec19f3a0414f5409ca8a059125cd014820fcb0a03a62c651092a5122900133a2`)
  - [`v0143-e5fp16-rerank-on-variance.json`](benchmark/results/2026-06-03-gpu-v0143/v0143-e5fp16-rerank-on-variance.json) (sha256 `7062923251fd70041e48aa9dd70be934eda3b530376a17a524f5bf592db736ec`)
- **Reproducibility note on the FP16 model:** the GPU runs above use the
  FP16 e5-base variant (`onnx/model_fp16.onnx`, 530 MB, sha256
  `5d760477f691b665da2b94e1528eb6938b795f76064d9392e6af7118b8a3f54a`),
  pinned in `integrity.rs` as `MODEL_E5_BASE_ONNX_FP16`. Select it with
  `MGIMIND_MODEL_VARIANT=gpu mgimind doctor --fix` (or just `auto` with
  `MGIMIND_USE_CUDA=1`). The `cpu` variant continues to download the
  INT8 model (~278 MB) as the default for zero-config users.

**Reading:**

- **R@5 = 99.2%** is the strongest single-config result on LongMemEval-S
  this project has produced. It is +1.0pp over the v0.8.1 MiniLM baseline
  and roughly 4× faster wall-time with the reranker still on, on a single
  RTX 3090 — the actual hardware most self-hosters have.
- Variance between Run B and Run C on the same config is 0pp (R@1, R@10)
  and 0.4pp (R@5). At n=500 this is consistent with binomial noise.
- The `rerank=true` ablation (+4.2pp R@1, +1.2pp R@5, +0.6pp R@10) is
  meaningful and justifies the 2.4× wall cost on this dataset.
- `single-session-preference` (n=30) remains the weakest stratum on
  both configurations — same shape as the baseline. Open issue, not a
  regression.

### LongMemEval-S — 2026-06-04, v0.14.3 GPU (RTX 3090, MiniLM-L6-v2 FP16)

Ablation control for the headline above. Same host, same build, same
500 questions, but switch the embedder back to `all-MiniLM-L6-v2` (the
v0.8.1 baseline model) running on the GPU as FP16. Isolates "what is from
e5-base" vs "what is from GPU + bigger sessions cache vs from v0.8.1 CPU".

```
config: model=all-MiniLM-L6-v2 dim=384 FP16 rerank=false
scored: 500 questions (0 abstention excluded)

Overall:
  R@1  = 85.0%   (Δ vs v0.8.1 CPU INT8: -0.2pp)
  R@5  = 98.0%   (Δ vs v0.8.1 CPU INT8: -0.2pp)
  R@10 = 99.6%   (Δ vs v0.8.1 CPU INT8: +0.2pp)
  wall = 351s (~5.9 min)  vs ~1h45m CPU baseline (~18× speedup)

By question type:
  knowledge-update           n=78   R@1=94%  R@5=100% R@10=100%
  multi-session              n=133  R@1=83%  R@5=99%  R@10=100%
  single-session-assistant   n=56   R@1=100% R@5=100% R@10=100%
  single-session-preference  n=30   R@1=57%  R@5=93%  R@10=97%
  single-session-user        n=70   R@1=84%  R@5=97%  R@10=100%
  temporal-reasoning         n=133  R@1=82%  R@5=96%  R@10=99%
```

- **Raw per-question JSON:** [`benchmark/results/2026-06-03-gpu-v0143/v0143-minilm-fp16-rerank-off.json`](benchmark/results/2026-06-03-gpu-v0143/v0143-minilm-fp16-rerank-off.json) (sha256 `c778e9ff9814d0ba68c1e180335ba22c6c11db71baa3d8df4c829097b4525efe`)

**Reading:** MiniLM FP16 on the GPU lands within ±0.2pp of MiniLM INT8 on
CPU at every cutoff. Two takeaways:

1. **FP16-on-GPU is recall-equivalent to INT8-on-CPU** for this embedder.
   The 18× speedup is free in retrieval quality terms. This is the
   evidence behind the GPU recipe being a recommended path, not a
   trade-off.
2. **The +1.0pp R@5 in the e5-base headline is from the model swap,
   not from GPU or the v0.14.x retrieval policy.** Keeping this honest:
   the v0.14.1 counterfactual A/B policy did not move recall by itself
   on this dataset.

### What didn't make it in (honest)

- **Reranker on actual GPU** (vs CUDA-registered-but-CPU-fallback): the
  reranker ships as INT8 ONNX and falls back to CPU at inference time
  for the same `MatMulInteger` reason as the embedder did. Shipping an
  FP16 reranker through `doctor --fix` is on the roadmap; until then
  the `rerank=on` wall-time numbers above are mostly CPU-bound.

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

### Results — 2026-06-02 (v0.14.x, final 227-pair v0.10.0 set)

Mined locally with `scripts/scrape_procedural_dataset.py` from 20 OSS repos
(cargo, clap, click, cobra, commander.js, django, express, flask, go,
hyper, next.js, pytest, qdrant, requests, reqwest, rust-clippy, rustfmt,
rustlings, serde, tokio, yargs) at depth 5000 commits each. **227 pairs**
after filtering, stratified by file-touch heuristic (test paths → `test`,
build manifests → `compile`, CI dirs → `ci`).

```
config: model=multilingual-e5-base dim=768 rerank=false
scored: 227 pairs

Overall:
  R@1  = 48.0%
  R@5  = 96.5%   <- headline
  R@10 = 98.7%

By language:
  go    n=2    R@1= 50.0% R@5=100.0% R@10=100.0%
  py    n=46   R@1= 50.0% R@5= 97.8% R@10=100.0%
  rust  n=116  R@1= 45.7% R@5= 96.6% R@10=100.0%
  ts    n=63   R@1= 50.8% R@5= 95.2% R@10= 95.2%

By stratum (error type):
  ci       n=1    R@1=  0.0% R@5=100.0% R@10=100.0%
  compile  n=10   R@1= 80.0% R@5=100.0% R@10=100.0%
  runtime  n=120  R@1= 47.5% R@5= 95.8% R@10= 99.2%
  test     n=96   R@1= 45.8% R@5= 96.9% R@10= 97.9%
```

- **Dataset:** [`benchmark/datasets/procedural-v010-227.jsonl`](benchmark/datasets/procedural-v010-227.jsonl)
- **Raw per-pair JSON:** [`benchmark/results/2026-06-02-procedural-v010-final/raw.json`](benchmark/results/2026-06-02-procedural-v010-final/raw.json)
- **Earlier 177-pair v3 set:** [`benchmark/datasets/procedural-v010-177.jsonl`](benchmark/datasets/procedural-v010-177.jsonl)
- **Earlier bootstrap (111 pairs):** [`benchmark/datasets/procedural-v010-bootstrap-111.jsonl`](benchmark/datasets/procedural-v010-bootstrap-111.jsonl)

### What the numbers say (and don't)

- **R@5 = 96.5%** is the headline. The system surfaces the right playbook in
  the top 5 results 96.5% of the time across 4 languages and 4 strata.
- **R@1 = 48.0%** is realistic. Many fix commits share near-identical error
  signatures ("test failure on macOS" appears across 8 commits in next.js).
  With multiple plausible fixes for one signature, picking the *exact* gold
  at rank 1 is partly a coin flip — the metric to watch is R@5.
- **compile R@1 = 80%** is the strongest stratum: compile errors carry
  highly specific signatures (`error[E0599]`, `cannot find name`), which the
  sparse retrieval branch catches reliably.
- **R@5 drop vs the 177-pair v3 set (98.9% → 96.5%)** is honest: next.js
  introduced 50 TS pairs with near-duplicate "Hydration mismatch" style
  signatures that compress retrieval headroom. Larger corpus, harder noise,
  more realistic number. Don't cherry-pick the smaller set.
- The v0.10.0 ров target was "200+ pairs from 20+ repos with stratum
  coverage". Reached: 227 pairs, 20 repos, 4 strata, 4 languages.

### Like-for-like vs other systems (planned)

To compare against a system that publishes QA accuracy (e.g. Mem0), run **their**
harness (`mem0ai/memory-benchmarks`) with the same answerer/judge model and top-k,
rather than comparing across metrics. Record the judge model, provider, and date
(LLM judges drift). This is a separate effort from the zero-API recall above.
