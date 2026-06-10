# ADR 0007: Extractor model choice (Qwen, not Granite or Phi)

**Status:** accepted.
**Date:** 2026-06-10.

## Decision

The auto-extractor's default local model is **Qwen 2.5 Instruct** (1.5B Lite /
3B Default, Q4_K_M GGUF, CPU-first via a bundled `llama-server` subprocess from
llama.cpp). IBM Granite and Microsoft Phi were tried and dropped. A cloud extractor (Gemini flash) is used **only** for
the STALE benchmark, never in the product path.

## Context

The auto-extractor reads raw conversation turns and writes structured facts
(`mind_fact add`) underneath the memories. It runs locally so the default path
stays no-cloud. The model has to do slot-filling extraction well enough that the
validity model (duel rule, supersession) has correct facts to resolve. A weak
extractor poisons everything downstream: a perfect duel rule still scores near
zero if the facts never got extracted.

We went through three models before settling.

### Granite (first try, dropped)

IBM Granite 3.3 (2B / 8B) was the first local extractor, on the
`stale-extraction-optimization` branch. The 2B's recall on long sessions was
effectively zero, and that, not the duel rule, dominated the early STALE
numbers. The lesson, recorded at the time: **Granite was the bottleneck, not the
mechanism.** A deterministic, LLM-free test of the duel rule scored 6/6 (100%)
while the end-to-end number sat at ~2% purely because the 2B extractor missed
the facts. Chasing extractor recall was measuring the wrong layer.

### Phi (second try, dropped)

Phi was tried as a Granite replacement. It failed the same way on the part that
matters: both local small models were blind to T2 (propagated) indirect signals
and hallucinated slots that weren't in the text. Phi is not in the codebase; it
was a rejected experiment.

### Qwen (chosen for the product)

Qwen 2.5 Instruct is the default, run through a bundled `llama-server`
subprocess over localhost. Same family for both size variants (identical chat
template, tokenizer, and output structure), so the Lite/Default switch is a
config flag over one inference path. The 3B has native RU + EN + ZH, which the
author's mixed-language base needs. CPU-first for the same reason as the
embedder: one binary, no CUDA at build, no driver at runtime, works everywhere.
Q4_K_M keeps the download small (~1–2 GB) and the per-extraction latency in the
single-digit-seconds range on a current x86 CPU.

### Gemini cloud — benchmark only

The STALE run used `gemini-flash-latest` for extraction because no local 2–8B
model cleared the T2 bar, and that lifted T2 from 22% to 70% on a curated sample.
This is a **benchmark-only** choice and is documented as such in BENCHMARKS.md. It
does not move into the product, for a reason that is a hard invariant.

## The invariant this records

This ADR fixes the rule for the Qwen path: **feed raw turns to our own
extractor, never pre-extract facts through a cloud API in the product.** (The
STALE harness on the `stale-extraction-optimization` branch calls the same idea
"A2", worded there around the Granite extractor; this is the canonical record
for the shipped Qwen path.) If the product used a cloud LLM to extract facts, the
system would be measuring "our duel rule on top of GPT/Gemini extraction", a
different and incomparable thing, and the default path would stop being no-cloud.
So a cloud extractor is acceptable as a benchmark instrument, to isolate the duel
rule from local-model recall, while staying off the product default. The product
stays honest end-to-end on a local model; the benchmark may swap the extractor to
separate the layers, with the swap disclosed.

## Consequences

**Plus:**

- The default install is no-cloud end to end: local embeddings, local rerank,
  local extraction.
- One model family for both size tiers keeps the extractor a single code path.
- The Granite/Phi dead ends are recorded, so the next person does not re-run
  them expecting a different result on the T2 axis.

**Minus:**

- A local 1.5–3B extractor has real recall limits on long, indirect-signal
  sessions (the T2 weakness). The product accepts this; the benchmark works
  around it with a disclosed cloud swap.
- The Granite extractor still lives on the `stale-extraction-optimization`
  branch (open PR). It is experimental and not the product default; this ADR is
  the record that it lost to Qwen.

## Lesson (kept for the next round)

Keep the three layers separate when evaluating: (a) duel-rule validity as a
deterministic, LLM-free test (the project's actual IP, a real 100%-or-bug
number); (b) extractor recall as its own metric, honestly captioned as
small-local-model recall; (c) end-to-end as the product. Folding them into one
number measures the extractor's mood, not the mechanism.

## References

- Code: `src/extractor.rs` (Qwen variants, CPU-first, `is_llama_server_installed`
  gate). The Granite variants live on branch `stale-extraction-optimization`.
- Benchmark: `BENCHMARKS.md` (STALE section — the disclosed cloud-extractor swap),
  `src/bench_stale.rs`.
- Companion: [0006](./0006-derived-state-provenance.md) (the validity model the
  extractor feeds).
