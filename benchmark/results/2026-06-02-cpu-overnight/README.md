# CPU overnight bench — 2026-06-02

**Host:** madgodinc@pop-os
**HW:** Intel i5-12400F (6c/12t), 48GB RAM
**Note:** CPU-only run; 5060 Ti present but NOT used (CPU is the default user path)
**mgimind:** v0.8.1 main branch (commit a5fb6e4)
**Dataset:** LongMemEval-S (longmemeval_s.json, 500 questions, EN)
**Isolated runtime:** MGIMIND_HOME=~/mgimind-bench, qdrant on port 6336 (separate from prod 6334)

## Smoke
- e5-base (multilingual, 768): 1q = 32s wall, 3m07s CPU
- MiniLM-L6-v2 (EN, 384): 1q = 11s wall, 1m05s CPU

Est. full 500q run:
- e5-base: ~4h
- MiniLM-L6-v2: ~90min

## Plan

| # | Model | dim | rerank | Repeats |
|---|-------|-----|--------|---------|
| 01-02 | all-MiniLM-L6-v2 (EN headline) | 384 | off | ×2 (variance) |
| 03 | all-MiniLM-L6-v2 | 384 | on | ×1 |
| 04 | multilingual-e5-base | 768 | off | ×1 |
| 05 | multilingual-e5-base | 768 | on | ×1 |
| more | per remaining time | | | |

## Results

| Run | Model | dim | rerank | Started | Wall | R@1 | R@5 | R@10 | Notes |
|-----|-------|-----|--------|---------|------|-----|-----|------|-------|
