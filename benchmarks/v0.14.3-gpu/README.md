# v0.14.3 GPU bench — 2026-06-03/04

500 questions, LongMemEval-S, RTX 3090 (RunPod secure pod).

## Configuration

- mgi-mind v0.14.3 (counterfactual A/B retrieval policy; pre-v1.4 architecture)
- Embedding: `multilingual-e5-base`, FP16 (downloaded from Xenova on HF)
- Reranker: `bge-reranker-base` (INT8; falls back to CPU on rerank, see note)
- ORT 1.24.2 GPU build with CUDA execution provider
- `MGIMIND_USE_CUDA=1`

## Runs

| # | Embedding | Rerank | R@1 | R@5 | R@10 | Wall | raw.json sha8 |
|---|-----------|--------|-----|-----|------|------|---------------|
| 1 | e5-base FP16 | off | 88.4% | 98.0% | 99.4% | 629s | 73a1e491 |
| 2 | e5-base FP16 | **on** | **92.6%** | **99.2%** | **100.0%** | 1539s | ec19f3a0 |
| 3 | e5-base FP16 | on (variance check) | 92.6% | 98.8% | 100.0% | 1528s | 70629232 |
| 4 | MiniLM FP16 | off | 85.0% | 98.0% | 99.6% | 351s | c778e9ff |

Run 2 is the headline. ±0.4pp R@5 variance between runs 2 and 3.

## Comparison baseline (v0.8.1 CPU)

| # | Embedding | Rerank | R@1 | R@5 | R@10 | Wall | Hardware |
|---|-----------|--------|-----|-----|------|------|----------|
| - | MiniLM INT8 | off | 85.2% | 98.2% | 99.4% | 1h45m | RTX 5060 Ti (CPU path) |

Δ vs baseline (run 2): **+7.4pp R@1, +1.0pp R@5, 4× wall-time speedup**.

## Ablation conclusions

1. **MiniLM FP16 GPU ≈ MiniLM INT8 CPU.** R@5 98.0 vs 98.2 is noise. GPU buys 18× speed without changing recall. (Run 4 vs baseline.)
2. **v0.14.x retrieval policy does NOT improve recall** in isolation. MiniLM in v0.14.3 gives the same numbers as v0.8.1.
3. **The +1.0pp R@5 lift to 99.2% comes from**:
   - (a) switching to e5-base (+0pp on its own — see run 1 vs baseline; e5 = MiniLM at top-K=5)
   - (b) enabling reranker (+4.2pp R@1, +1.2pp R@5, +0.6pp R@10 — see run 1 vs run 2)
4. **Reranker is the dominant factor**, not the architecture changes in v0.14.x.

This is the honest framing for v1.0 release notes: "the headline R@5 = 99.2% on this hardware/model config; the architecture changes in v0.14 do not contribute to the number, and the v1.4 / v1.5 model changes (duel rule, doubt window, install-mode) need their own R@k regression check that has not been run yet."

## Reranker GPU note

We did NOT find a FP16 version of `bge-reranker-base`. Even with `--features cuda` and CUDA EP registered, the reranker INT8 ops fall back to CPU. That's why `rerank=on` (1539 s) is 2.4× slower than `rerank=off` (629 s) on the same e5-base run — the embedding side runs on GPU, the rerank side does not.

## Costs

| Item | Cost |
|------|------|
| RunPod secure pod (RTX 3090, 5h) | $1.12 |
| RunPod community pod #1 (lost data after 30h, provider reclaimed) | $11 |
| Total project | $12.12 |

The lost pod is documented in [`feedback-runpod-community-risk`](../../../Brain/memory/feedback_runpod_community_risk.md) — community cloud is fine for one-shot short jobs, but anything > 2 hours needs secure or you'll lose the data when the host's primary user returns.

## Data

Raw `results.json` is in this directory. Per-question outputs are kept off-repo at `~/Brain/mgi-mind-v2/benchmark/results/2026-06-03-gpu-v0143/raw-{run-number}.json` because they include verbatim memory content from the LongMemEval dataset; the JSON in this folder is the aggregated headline-numbers-only version.
