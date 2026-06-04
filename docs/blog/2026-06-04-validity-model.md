# What I changed about AI agent memory in v1.4 / v1.5 / v1.6 — and what I haven't measured yet

*Draft. 2026-06-04. Honest-limits-first per reverse-formula: scroll to the bottom if you want the headline benchmark, this post does not bury caveats.*

---

## What this is not

This post does not claim mgi-mind v1.6 is the best AI-agent memory layer. It does not have a fresh R@k number against mem0 / Zep / Graphiti. It does not have a paper. The benchmark sweep from issue #16 needs an LLM judge API key I do not have yet. The architecture changes from v1.4-v1.5 (duel rule, doubt window, install-mode profiles, active re-test pass) have **not been calibrated against LongMemEval**. Until they are, the headline number is still from v0.14.3:

> **R@5 = 99.2%** on LongMemEval-S (500 questions), multilingual-e5-base FP16 + bge-reranker, RTX 3090.

That number does not change in v1.6 — the v1.4 changes are about *what memory means*, not *how it's retrieved*. Retrieval improves later (issue #16). What changed is the model.

## The problem that drove v1.4

Default RAG memory accumulates contradictions like garbage. Say you log "I use Rust" today. Tomorrow you switch to Go and log "I use Go". The old fact stays in the store. Search returns both, ranked by cosine similarity. The model has no signal that the new fact supersedes the old one.

This is the same problem the original [audit #13](https://github.com/madgodinc/mgi-mind/pull/2) flagged. A contributor (@spikefcz) proposed a fix: a `single_valued_predicates` list in config that auto-supersedes single-valued facts. Narrow, correct, would have shipped in a single afternoon.

I closed the PR.

Not because it was wrong, but because the same machinery needs to handle three other problems and a flat list does not generalize:

1. **Some predicates are honestly multi-valued.** "Mad uses Rust" + "Mad uses TypeScript" are both true. A single-valued treatment incorrectly conflicts them.
2. **Some predicates are temporally single.** "Lives in Prague" was true; "Lives in Dublin" is true now. Both should be queryable; only Dublin should rank in the default view.
3. **Some single-valued facts deserve to stay live.** "Email is `mad@example.com`" might be a typo overwriting a real address; you do not want to silently invalidate the real one on the first write.

The v1.4 fix is a `Cardinality` enum (`Single` / `TemporalSingle` / `Multi`) on every predicate, plus a duel rule that decides which fact wins on contradiction. The duel rule reads cached signals — `dependants_count`, `confirmations_count`, `external_signals` — and ranks the incumbent against the challenger. The loser is **dampened** (gets `valid_until` set), never deleted. Mechanism 1 invariant.

`<details><summary>Why never delete</summary>` Three reasons:

1. **Future evidence can reverse the duel.** If the loser was actually right and you delete it, you cannot recover.
2. **Audit trail.** When the model behaves unexpectedly months later, you want to grep the audit log and see exactly when the fact flipped.
3. **STALE benchmark behavioural metric.** `state_resolution` and `premise_resistance` need the loser still readable; deletion makes them silently zero.

`</details>`

## §6 — install-mode profiles (v1.5 Phase 6)

The duel rule weights three signals to compute a fact's confidence score:

```
confidence_score = w_dependants * dependants_norm
                 + w_confirmations * confirmations_norm
                 + w_external * external_norm
                 - inheritance_discount_penalty * (1 if inherited else 0)
```

What weights? Depends on what the memory is *for*. If you're using mgi-mind as a single-user chat-assistant memory (the default), `confirmations` is decoratively weak — one person saying the same thing twice is almost no evidence. `dependants` (how many other facts structurally depend on this one) is what's load-bearing.

If you're using it as a CI loop's memory (test outcomes flow in via `mind_outcome`), `external` is the strongest signal — a passing test is much harder to lie about than a conversational repetition.

If you're using it as a multi-tenant store (multiple distinct agents writing), `confirmations` becomes load-bearing again — but for a different reason: independent agents reaching the same conclusion is real evidence, not the same agent repeating itself.

v1.5 ships three illustrative anchor profiles:

| mode | dependants | confirmations | external |
|------|------------|---------------|----------|
| `chat-only` (default) | 0.7 | 0.1 | 0.2 |
| `dev-with-ci` | 0.5 | 0.15 | 0.35 |
| `multi-tenant` | 0.4 | 0.4 | 0.2 |

Per-mode anchors picked via `mgimind config install-mode`. Auto-detect heuristic runs on first `mgimind serve` and recommends a mode based on observed signal counts; it never auto-applies (mis-classification cost is silent quality drift). `mgimind doctor` surfaces both the current mode and the recommendation.

The auto-detect uses two counts:

- `external_signal_count_last_7d >= 10` → DevWithCi
- `distinct_session_agents_last_30d >= 3` → MultiTenant
- else ChatOnly

Honest about the numbers: those anchors are starting points. They are marked `TODO(phase-4-calibration)` in the source. A real sweep against the STALE benchmark will move them. The contract test pins `weight_new_for_mode(_, ChatOnly)` against the legacy `weight_new` bit-for-bit, so the v1.4 / v0.14.x retrieval surface is unchanged for the default mode.

## §10 q5 — three guarantees on the background loop (v1.5 Phase 8)

The doubt window (v1.4 Phase 3) introduces an anti-ossification mechanism: a fact that gets retrieved without confirming context drift starts a counter; after N drifted retrievals, the fact enters the doubt window and its confidence is halved in ranking until a fresh non-drifted retrieval resets the counter.

The blind spot: facts that are *never* retrieved never enter the doubt window. A background pass has to actively re-test top-N entrenched facts. This was scaffolded in v1.4 (`spawn_background_retest_loop`) but the body was `n_processed_this_tick = 0` — a placeholder.

v1.5 Phase 8 closes the loop. Three hard guarantees from §10 question 5 of the synthesis:

**(a) Never concurrent with an MCP tool call.** A `BusyGuard` raises `MCP_BUSY` on every tool dispatch. The background loop checks `is_mcp_busy()` at outer wake AND between facts inside the walk. A tool call starting mid-tick breaks the walk early and re-flags any unprocessed candidates for next tick.

**(b) Hard per-tick cap.** `select_retest_candidates(_, BACKGROUND_PER_TICK_CAP)` returns at most 50 ids per tick. An assert at the end of the walk panics the background task (which auto-restarts via outer scheduling) if a refactor breaks the cap. Worse than wrong is silently scanning unbounded work and starving the MCP path.

**(c) Load-aware cadence.** `loadavg_multiplier()` reads `/proc/loadavg` on Linux and returns `2.0` when the 1-minute load exceeds `1.5 × num_cpus`. The cadence formula doubles, the loop sleeps longer. On non-Linux it returns `1.0` (the loop runs without a back-off signal; v1.7 will add Windows / macOS equivalents).

For each candidate the loop:
1. Reads payload via a single batched `get_points` (one round-trip — v1.6.0 changed this from 4 separate fetches).
2. Computes new `confidence_score` with the current install-mode anchors.
3. Compares to cached score. Three transitions possible via `decide_retest_transition`:
   - **PromoteToDoubt** — both `delta < -0.2` AND `new < 0.3`. Two independent reasons must agree before state changes. Avoids flipping mid-band facts on noise.
   - **RecoverFromDoubt** — already in doubt AND `delta > +0.2`.
   - **NoChange** — write back new score, continue.
4. Writes audit entry on every transition (`AuditOp::RetestPromote` / `::RetestRecover` — `NoChange` is intentionally not logged to keep the file from ballooning).

**`RetestTransition` enum is exhaustive: there is no `Remove` variant.** Mechanism 1 invariant lives in the type system, not just convention.

## What v1.6.0-1.6.3 cleaned up

The v1.5 release notes declared "Honest limits". v1.6 closes three of them:

- **v1.6.0** — batched payload reads in `retest_fact_step82` (4× round-trip reduction per fact), `cited_by` chain following (the v1.5 self-citation guard always blocked because the lookup was stubbed `|_| None`), integration tests on `spawn_background_retest_loop` at the registry / scheduling level.
- **v1.6.1** — CLI surfaces for `mind_outcome`, audit log filters by op and time window, install-mode weight breakdown in `doctor`, fact graph distribution in `stats`.
- **v1.6.2** — `mgimind stats --json` for monitoring scripts, `mgimind facts list/show` for KG inspection.
- **v1.6.3** — `mgimind migrate-v14 cardinality --apply` for bulk-registering predicate cardinalities after extraction (Mad's base mid-extraction shows 1113 distinct predicates, 1096 high-confidence proposals), `bench-stale` + `bench-stale-sweep` CLI scaffold, CONTRIBUTING.md + CODE_OF_CONDUCT.md + issue templates.

Total: **290 unit + 6 integration tests, 0 failed.** The build is warning-free.

## What I haven't measured yet (honest limits)

This is the part the reverse-formula puts on top so you can stop reading if it disqualifies the post for you:

- **STALE bench calibration is not run.** The architecture changes need their own `R@5 regression < 1.0pp` gate. Tooling scaffold exists; the dataset adapter and judge model are TBD. Tracked in issue #16.
- **QA accuracy bench is not run.** Different metric than R@k — "with the retrieved memory, did the model answer correctly". Needs OpenAI / Anthropic API key. Tracked in issue #17.
- **Constants are illustrative.** Every `pub const` in the v1.5 / v1.6 code carries a `TODO(phase-4-calibration)` comment. Real sweep against the bench will tune them. Defaults are picked from the synthesis document (§6 anchors) and lit review (`DUEL_FLIP_RATIO = 1.5` from STALE Appendix G).
- **Mac and Windows binaries are not in v1.5 / v1.6.** Issues #19 and #20 are open. If you're on either platform and want them, drop a comment with use case.
- **`cited_by` self-citation guard depends on cached confidence_score.** Facts that haven't been through the retest pass yet read as confidence 0.5 (default), so they default-block. This is the conservative behaviour; v1.7 may add a separate "pre-rest" baseline if it's surfaced as a problem.
- **The benchmark headline (R@5 = 99.2%) is from v0.14.3**, predating these architecture changes. v1.4-v1.6 retrieval gives the same number by construction (the formulas only kick in when there is contradiction). The R@5 number rises only when calibration adjusts the formula anchors against real data.

## What you can run today

```sh
# Install (Linux x86_64, ~25 MB stripped):
curl -L https://github.com/madgodinc/mgi-mind/releases/latest/download/mgimind-x86_64-unknown-linux-musl.tar.gz | tar -xz

# Initialize and start the MCP server:
./mgimind init
./mgimind serve &

# Or via CLI directly:
./mgimind add projects "I switched from Toolong to mgi-pulse for log navigation"
./mgimind search "log viewer"

# v1.5 surfaces:
./mgimind config install-mode             # current + auto-detect
./mgimind outcome <memory_id> test_passed --source ci.github.com/run/12345
./mgimind audit list --op retest_promote --since-hours 24
./mgimind stats --json | jq .kg_facts
./mgimind facts list --limit 10 --with-id
./mgimind facts show <fact_id>
```

If you want to follow along: [Discussions](https://github.com/madgodinc/mgi-mind/discussions) was just enabled. PR templates are in `.github/`. CONTRIBUTING.md describes the branch model.

Issues + Discussions are how I find out what I should be measuring next.

---

## Appendix: links

- [Source](https://github.com/madgodinc/mgi-mind)
- [v1.5 release notes](https://github.com/madgodinc/mgi-mind/releases/tag/v1.5.0)
- [v1.6.3 release notes](https://github.com/madgodinc/mgi-mind/releases/tag/v1.6.3)
- [Benchmark headline numbers](https://github.com/madgodinc/mgi-mind/tree/main/benchmarks)
- [Audit #13 fix history](https://github.com/madgodinc/mgi-mind/pull/2)
- [Roadmap issues](https://github.com/madgodinc/mgi-mind/issues)

The original synthesis doc that drove these designs is local-only (~42 KB of internal critic rounds), not in the repo. If anyone wants to read it, open a Discussion and I will paste it.
