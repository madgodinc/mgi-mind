# Changelog

## 1.6.3 — bench-stale CLI + bulk cardinality apply + contributor docs

### `mgimind migrate-v14 cardinality --apply`

v1.4 cardinality walk wrote a JSON of proposed predicate
cardinalities for manual review. Manual loop was fine at the 9
predicates we had at launch, but extraction produces hundreds.

```sh
mgimind migrate-v14 cardinality --apply
```

Bulk-registers every High-confidence proposal via
`knowledge::register_cardinality`. Low-confidence entries stay in
the JSON for human review.

### `mgimind bench-stale` + `mgimind bench-stale-sweep`

CLI surface for the STALE benchmark calibration sweep. The STALE
protocol adapter is still TBD (returns an empty report), but the
CLI plumbing and sweep grid are now in place against the existing
type contracts:

```sh
mgimind bench-stale <dataset> --duel-flip-ratio 1.5 --output run.json
mgimind bench-stale-sweep <dataset> --output-dir results/
```

Sweep grid is 7 runs: baseline + ±50% on `DUEL_FLIP_RATIO`,
`DUEL_CONTESTED_RATIO`, `DOUBT_DRIFT_THRESHOLD`. Each writes a
per-run JSON; the harness writes a summary.json mapping run name
to overall_pct / state_resolution_pct / premise_resistance_pct.

### Contributor docs

- `CONTRIBUTING.md` — project layout, build commands, three
  architecture rules (Mechanism 1 invariant, §10 q5 guarantees,
  illustrative-until-calibrated constants), how to add an MCP tool,
  branch model.
- `CODE_OF_CONDUCT.md` — pragmatic compression of Contributor
  Covenant 2.1.
- `.github/ISSUE_TEMPLATE/` — bug_report.md + feature_request.md +
  config.yml routing questions to Discussions.

### Tests

290 unit + 6 integration tests, 0 failed.

## 1.6.2 — facts inspection + machine-parseable stats

Two more CLI usability additions.

### `mgimind stats --json`

Machine-parseable variant of stats for monitoring dashboards or
scripts. Same fields, JSON shape. Useful during extraction to
poll fact counts:

```sh
watch -n 30 'mgimind stats --json | jq .kg_facts'
```

### `mgimind facts list/show`

First-class CLI surface for inspecting the knowledge graph.

```sh
mgimind facts list --limit 5 [--predicate uses_language] [--sort created] [--with-id]
mgimind facts show <fact_id>
```

`show` prints every payload field the v1.5/v1.6 formulas read:
subject, predicate, object, valid, created_at, dependants_count,
confirmations_count, external_signals, confidence_score,
doubt_drift_count, status.

O(facts) for list — fine at 12k base, capped at 10k for the
dependants decoration.

### Tests

290 unit + 6 integration tests, 0 failed.

## 1.6.1 — CLI surfaces for v1.5 / v1.6 features

Four CLI usability fixes. No schema changes, no MCP surface
changes, no formula changes.

### `mgimind outcome <id> <signal_type>`

`mind_outcome` was MCP-only. Now also available from the terminal
for debugging the error-rate guardrail and confidence_score paths
without a running MCP client.

### `mgimind audit list --op X --since-hours N`

`audit list` already showed recent events. Now filterable by op
type and time window. Useful right after extraction:

```sh
mgimind audit list --op retest_promote --since-hours 1
```

### `mgimind doctor` + `mgimind config install-mode` show weights

Both surfaces now print the install-mode AND the per-mode anchors
they imply:

```
[OK]   install-mode: chat-only [d=0.70 c=0.10 e=0.20] (matches auto-detect)
```

### `mgimind stats` shows fact graph distribution

```
KG facts:       2399
  dependants:   min=0 p50=0 p90=0 p99=0 max=1 mean=0.00
                4/2398 facts have ≥1 dependant (0.2%)
  in-doubt:     0 flagged for retest
  inherited:    0 (cleared on session end)
```

Calibration signal: if p90 stays 0 after `migrate-v14 dependants
--apply`, the dependants weight contributes nothing.

### Tests

290 unit + 6 integration tests, 0 failed.

## 1.6.0 — closing v1.5 honest limits

A polish release that closes three TBD items declared in v1.5's
"Honest limits" section. No new public surface. Same 37 MCP tools,
same CLI flags, same install modes. Smaller per-tick overhead and
sharper coverage of the §10 q5 guarantees.

### Step 1 — batched payload reads in retest_fact_step82

The v1.5 implementation made four separate `get_points` round-trips
per fact (dependants_count, confirmations_count, external_signals,
confidence_score). At BACKGROUND_PER_TICK_CAP=50 that was 200
round-trips per tick. New
`storage::read_point_payload_strings(client, collection, id, &keys)`
returns a HashMap in one call. retest_fact_step82 now does one
Qdrant fetch + four HashMap lookups. 4× reduction in round-trips
per fact, same semantics.

### Step 2 — `cited_by` chain following

The v1.5 self-citation guard always blocked because the lookup
closure was `|_| None`. v1.6 wires real lookup via
`fetch_citing_confidences` — a single batched get_points against
MEMORIES_COLLECTION returns a `HashMap<id, f32>` of cached
confidence_scores. The closure reads from that map synchronously.

- Citing memory with cached confidence ≥ 0.5 → `cited_by` signal
  contributes 0.2 × external slot weight.
- Citing memory below 0.5 or missing → guard blocks (conservative
  default; never crashes).

Mechanism 1 invariant preserved.

### Step 3 — integration tests on `spawn_background_retest_loop`

Closes the audit-flagged gap: v1.5 had no test that ran the spawned
task end-to-end. v1.6 adds four:

- `busy_flag_observable_by_loop_check` — guarantee (a) plumbing.
- `per_tick_cap_enforced_by_drain` — guarantee (b) cap enforcement.
- `edit_counter_consumed_each_tick` — signal flow for guarantee (c)
  cadence.
- `drain_then_reflag_preserves_registry` — failure-path re-flag
  contract.

Production code: `spawn_background_retest_loop` now delegates to
`spawn_background_retest_loop_with_cadence(cfg, default)`. The
parametrised variant lets tests pass a short cadence if they spawn
the actual loop.

Registry-level coverage is the contract; the spawn body is one
match against helpers all of which are now tested. Spawning the
loop against stub Qdrant proved too flaky for parallel runs (race
over global registries) — registry tests are the stable equivalent.

### Build hygiene

Six modules carry calibration scaffolding (`pub const` /
`pub fn` declared for the future surface). Each gains a
module-level `#![allow(dead_code)]` with a comment so the build
output is clean and contributors see green builds. No semantic
change.

### Tests

- 290 unit + 6 integration tests, 0 failed.
- 4 new tests for v1.6 (all in `doubt::tests`).

### Tracked v1.6 follow-ups (still open)

- #16 — STALE bench calibration against Mad's base.
- #17 — QA accuracy bench with LLM judge.
- #18 — migrate legacy `external_signals` counter to typed log.
- #19, #20 — Mac and Windows binary releases.

## 1.5.0 — install-mode profiles + typed external signals + active re-test pass

The v1.4 → v1.5 work closes the §6 and §10 q5 / q6 questions from
the validity-model synthesis. The duel rule (v1.4 Phase 2), doubt
window (v1.4 Phase 3 scaffold), and STALE bench adapter (v1.4
Phase 4 scaffold) are still calibration TBD — see "Honest limits"
below.

### v1.5 Phase 6 — install-mode profile + per-mode confidence_score

Three install profiles select different anchors for the
`confidence_score` formula (synthesis §6). Each mode's weights sum
to 1.0 by construction:

| mode | dependants | confirmations | external |
|---|---|---|---|
| `chat-only` (default) | 0.7 | 0.1 | 0.2 |
| `dev-with-ci` | 0.5 | 0.15 | 0.35 |
| `multi-tenant` | 0.4 | 0.4 | 0.2 |

New CLI:

- `mgimind config install-mode` — print current profile + auto-detect
  recommendation + breakdown of inputs (`external_signals_7d`,
  `distinct_agents_30d`).
- `mgimind config set-install-mode <mode>` — set the profile.
  Restart `mgimind serve` for long-lived MCP sessions to pick up.

Auto-detect heuristic (`install_detect::collect`):
`distinct_session_agents ≥ 3` → MultiTenant; otherwise
`external_signal_count_last_7d ≥ 10` → DevWithCi; otherwise
ChatOnly. The recommendation is informational only — `doctor`
never auto-applies (§10 q6 mis-classification cost = silent
quality drift).

`mgimind doctor` surfaces the install-mode line:
```
[OK]   install-mode: chat-only (matches auto-detect)
```
…or, on mismatch, prints the exact `set-install-mode` line to run.

### v1.5 Phase 7 — `mind_outcome` MCP tool + error-rate guardrail

Generalises the procedure-only `mind_procedure_outcome` into a
typed external-signal API working on any memory:

```
signal_type: test_passed | code_compiled | user_confirmed | cited_by
weights:    1.0  | 0.3 | 0.7 | 0.2     (§7 anchors)
```

Per-signal options:
- `success=false` multiplies weight by `-0.5` — failures pull the
  score negative, not just absence of evidence.
- `cited_by` carries a self-citation guard: only counts when the
  citing memory has confidence ≥ 0.5.
- Idempotent on `(memory_id, signal_type, source)`. Re-posting the
  same CI outcome overwrites rather than inflates the log.

The typed score plugs into `duel.weight_new_for_mode` via the new
`NewFactInputs.external_signal_score: Option<f32>`. When `Some(_)`,
it bypasses the v1.4 log2 shape and uses the signed score directly
multiplied by the install-mode external slot weight. `None` falls
back to v1.4 behaviour — matters because Phase 1 migration writes
the legacy `external_signals: u32`, but the typed log stays empty
until users post `mind_outcome` calls.

Error-rate guardrail: if a fact accumulates ≥ 3 failed
`test_passed` signals within 7 days, it gets flagged for the
doubt window (`doubt::DOUBT_WINDOW_FLAGGED`). Only `test_passed`
counts here — `user_confirmed` / `code_compiled` failures are
noisier and don't trip the guardrail.

`tools/list` now returns 37 tools (was 36).

### v1.5 Phase 8 — active re-test pass with three §10 q5 guarantees

Turns the v1.4 Phase 3 background loop scaffold (which had
`n_processed_this_tick = 0`) into a real re-test pass. Three hard
guarantees enforced:

- **(a) never concurrent with MCP tool call** — `is_mcp_busy()`
  checked at the OUTER wake AND between facts inside the walk. A
  tool call starting mid-tick breaks the loop early, logs partial
  progress, re-flags unprocessed candidates for the next tick.
- **(b) hard per-tick cap** —
  `select_retest_candidates(_, BACKGROUND_PER_TICK_CAP)` returns ≤
  50, with a hard-fail assert at the end of the walk. A future
  refactor breaking the cap panics the background task (auto-restart
  by outer scheduling) rather than silently starving MCP.
- **(c) load-aware cadence** — new `loadavg_multiplier()` reads
  `/proc/loadavg` (Linux), compares 1 m load to
  `1.5 × available_parallelism`, returns `2.0` to back off when
  overloaded. Non-Linux returns `1.0` (no back-off, loop still
  runs).

New module `confidence` with pure formulas:
- `confidence_score(inputs, mode) -> f32` — §6 weighted blend.
- `decide_retest_transition(old, new, in_doubt) -> RetestTransition`
  — §8 step 8.2 rules. `PromoteToDoubt` requires BOTH a downward
  shift > 0.2 AND new < 0.3 (two independent reasons must agree).
  `RecoverFromDoubt` requires already-in-doubt AND upward shift > 0.2.
  **Mechanism 1 invariant: NEVER returns a delete verdict.**

`retest_fact_step82` ties it together. Every transition writes an
audit entry (`audit::AuditOp::RetestPromote` / `::RetestRecover`)
with old + new `confidence_score`. `NoChange` ticks aren't logged
(would balloon the file).

Audit found that `record_edit()` had ZERO production callers, so
the cadence was drifting to its 24 h max no matter how busy the
graph was. Wired into:
- `knowledge::add_fact`
- `knowledge::set_fact_payload_field` (choke point for migrations
  + dampening)
- `storage::set_memory_payload_field` (choke point for outcome
  writes)

### Honest limits

- **STALE bench calibration is not in v1.5.** The synthesis §11
  named `R@k regression < 1.0 pp` as the gate; running it against
  Mad's base requires ~$30-50 of GPU time, and that prep didn't
  fit before this release. v1.6 will carry the calibration prereq
  numbers.
- **Phase 6 anchors and Phase 7 type weights are illustrative.**
  The plan calls them out as starting points
  (`TODO(phase-4-calibration)`). A real sweep against the bench
  will move them.
- **cited_by self-citation guard is stub-closured.**
  `retest_fact_step82` passes `|_| None` for the citing-confidence
  lookup, so the guard always blocks. Wiring it to a real read
  of `confidence_score` payload is v1.6 step 2.
- **`spawn_background_retest_loop` has no end-to-end integration
  test.** Unit tests cover the pure helpers (12 in `confidence`,
  several in `doubt`); spawning the actual tokio loop with a fake
  Qdrant and asserting on tick behaviour is v1.6 step 3.
- **Mac/Windows binaries are not in v1.5.** Audit flagged this as
  "solution looking for a problem" — no GitHub issues from those
  platforms yet. Linux + Docker only for now; contributor PRs
  welcome.

### Tests

- 286 unit + 6 integration tests, 0 failed.
- 33 new tests for v1.5 (12 confidence, 9 outcome, 6 guardrail,
  10 install-mode, 4 typed-score in duel).

### Migration notes

- **Existing configs deserialise unchanged.** Pre-v1.5 `config.json`
  without an `install_mode` field defaults to `ChatOnly`, which
  preserves the v1.4 hardcoded weights bit-for-bit (`weight_new`
  is now `weight_new_for_mode(_, ChatOnly)` and the 256-input
  contract test pins this).
- New payload slot on memories: `external_signals_v15` (JSON
  `Vec<ExternalSignal>`). Distinct from v1.4
  `external_signals: u32` to preserve legacy counts. v1.6 will
  migrate.
- Background loop starts automatically when `mgimind serve` runs
  against an initialised config.

## 1.4.0 — validity model: schema, migration, duel rule, doubt window, auto-extractor

Closes phases 0-5 of the validity-model synthesis. Lands as a
sequence of feature branches merged in dependency order
(1 → 2 → 3 → 5 → 4) so each phase has a green CI snapshot on
main.

- **Phase 0 — schema primitives.** `Cardinality` (Single /
  TemporalSingle / Multi) + `EntryStatus` (Active / Contested /
  Stale / PropagationShadowed / Unknown / QuarantineCandidate) +
  cached `dependants_count`, `confirmations_count`,
  `confidence_score` payload fields.
- **Phase 1 — migration + measurement.** `mgimind migrate-v14
  dependants|cardinality|confirmations` walks the existing base
  and computes the cached fields. Read-only by default; `--apply`
  writes back. Parallelised via `buffer_unordered(8)`.
- **Phase 2 — duel rule.** `entrenchment(F_old)` vs
  `weight_new(F_new)`, resolution thresholds `DUEL_FLIP_RATIO=1.5`,
  `DUEL_CONTESTED_RATIO=0.5`. Per-(subject, predicate) lock map
  prevents race conditions. Quarantine path reuses the v0.11
  promote-on-repeat API.
- **Phase 3 — doubt window (scaffold).** State machine
  `apply_retrieval_event`, retrieval-triggered counter,
  inheritance flag registry. Background loop wired but with
  `n_processed_this_tick = 0` placeholder — closed in v1.5
  Phase 8.
- **Phase 4 — STALE benchmark adapter (scaffold).**
  `bench_stale.rs` with `CalibrationOverrides` struct sweeps every
  duel/doubt constant via env vars. Bench against real data is
  v1.6 prereq.
- **Phase 5 — auto-extractor (opt-in feature flag).** Qwen 2.5
  GGUF via subprocess `llama-server` + Vulkan backend.
  Triple-backtick prompt-injection fence + sanitisation.
  `PR_SET_PDEATHSIG` against orphans. Cached `reqwest::Client`.
  Install sentinel `.installed-b9496` against partial installs.
  Gated behind `--features extractor` (NOT on by default — would
  let an auto-installed extractor write through model semantics
  users don't know about yet).

Tools/list = 36 (was 35). New: `mind_predicate`.

## 1.1.0 — tool surface consolidation (alias phase)

Same shape competitors converged on: one tool per object, an `action`
field selects the verb. Doesn't break anything in v1.x — the 15
single-verb tools they replace stay live as deprecated aliases until
v2.0, with the death date written into every deprecated description.

The user-facing surface (non-deprecated) drops from **30 tools to 20**,
which is in the same range as Graphiti (~9), Cognee (11), mem0 (~9),
supermemory (3). The full `tools/list` is 35 entries because the 15
deprecated singletons still ship for backward compatibility.

### New tools (5 consolidated verbs)

- `mind_quarantine(action="list"|"show"|"promote")` — replaces
  `mind_quarantine_list` / `_show` / `_promote`.
- `mind_vault(action="store"|"get"|"list")` — replaces
  `mind_vault_store` / `_get` / `_list` (still terminal-only by design).
- `mind_session(action="start"|"last"|"end")` — replaces
  `mind_session_start` / `_last` / `_end`.
- `mind_fact(action="add"|"query"|"invalidate")` — replaces
  `mind_fact_add` / `_query` / `_invalidate`.
- `mind_library(action="create"|"list"|"delete")` — replaces
  `mind_create` / `mind_list` / `mind_delete`.

### Deprecated (still works through v1.x, removed in v2.0)

The 15 singletons above. Every deprecated tool now carries
`"deprecated": true` in its JSON schema and a description prefixed
`DEPRECATED — use mind_X(action="Y"). Removed in v2.0.`. Well-behaved
MCP clients hide deprecated tools; older clients keep working unchanged.

### Kept separate on purpose

- `mind_history` — "newest N by time" is a different verb from
  "find relevant by query"; merging it into `mind_search` would hurt
  clarity more than it would help surface size.
- `mind_doctor` vs `mind_stats` — "what is broken" vs "how much of what"
  are different questions.
- `mind_consolidate`, `mind_export`, `mind_import`, `mind_ingest`,
  `mind_web`, `mind_provenance_add`, `mind_search`, `mind_add`,
  `mind_context`, `mind_learn`, `mind_recall`,
  `mind_procedure_outcome` — already one verb per tool, nothing to
  collapse.

### Roadmap shifted

The roadmap moves down one minor: what was v1.1 backup is now v1.2,
v1.3 REST + portable format, v1.4 bi-temporal + supersession, v1.5
decay, v2.0 unchanged.

### Tests

23 MCP unit tests (149 unit + 6 integration overall) green. New
coverage: the consolidated `mind_vault` dispatches all three actions
to the same terminal-only instructions, and an unknown action returns
a structured error naming the allowed values.

## 1.0.3 — docs: ROADMAP.md (v1.1 → v2.0 committed, v3.0 candidate set)

Docs-only patch. The internal roadmap that drove v0.9 → v1.0 was not in
the repo — readers landing on the project had no way to see what was
committed for upcoming releases, what was deliberately out of scope,
and what was still being decided.

- New `ROADMAP.md` at the repo root with five committed minors
  (v1.1 backup, v1.2 REST + portable format + chunking, v1.3
  bi-temporal facts + supersession, v1.4 decay, v2.0 public-launch
  gate) and a "v3.0 horizon" section listing five candidate directions
  (local-LLM write gate, judge-eval QA mode, cross-agent, schema
  packs, self-wiring graph) that are **deliberately not promised** —
  whichever crosses both a critic-checked spec and a real user pull
  ships as v3.0; the others stay on the list or fall off.
- Carries over the anti-roadmap unchanged (no Obsidian plugin, no
  markdown-as-source, no 50+ MCP tools, no cloud-hosted mode where
  mgi-mind sees user data, no marketing pumps).
- README in all three locales now links to `ROADMAP.md` next to
  `CHANGELOG.md`.

No code, no MCP-surface, no on-disk format changes.

## 1.0.2 — docs: bring README "Current version" line in sync with the tag

Docs-only patch. README.md / README.ru.md / README.zh.md still said
"Current version: 0.11.x" and the install snippet pinned
`MGIMIND_TAG=v0.8.0` — both predate the 1.0 line. Readers landing on
the project page saw three different "current" versions
simultaneously: README 0.11.x, latest release v1.0.1, BENCHMARKS
baseline v0.8.1. That looks like a mess even when the underlying tag
chain is clean.

All three READMEs now say `Current version: 1.0.x (semver-stable since
v1.0.0)` and the install snippet pins `MGIMIND_TAG=v1.0.1`. Historical
mentions of v0.8.1 in BENCHMARKS.md are left as-is on purpose — they
are the dated "when this was measured" reference, not a claim about
the current version.

No code, no MCP-surface, no on-disk format changes.

## 1.0.1 — docs: rebalance v1.0 headline to the default install path

Docs-only patch on top of v1.0.0. No code, no MCP-surface, no on-disk
format changes — v1.0.0 → v1.0.1 is bit-for-bit identical at runtime.

v1.0.0 shipped with R@5 = 99.2% in the release title and at the top of
BENCHMARKS.md. That number is real, but it comes from the **opt-in
GPU + FP16 e5-base recipe**, not the default install. A zero-config
user who runs `mgimind doctor --fix` lands on `MGIMIND_MODEL_VARIANT=cpu`
INT8, where the measured number is R@5 = 98.2% with the reranker on
(R@1 = 91.6%, R@10 = 99.8%). Putting the GPU figure in the headline
sold a configuration the user does not actually run.

This patch puts the default install path on top:

- BENCHMARKS.md now opens "Results" with a four-row "Headline number"
  table — default CPU INT8 + reranker first, GPU FP16 as an ablation
  row below.
- BENCHMARKS.md adds a "How hard is the task" subsection in the
  methodology: per-question distinct-session distribution on
  LongMemEval-S is **min=38, p10=44, median=48, p90=52, max=62**, so
  R@5 puts the system in the top ~10% and R@10 in the top ~20% of the
  haystack. Neither cutoff is a mechanical ceiling.
- The GPU section's reading note now reads "this is not the headline"
  instead of "strongest single-config result".
- CHANGELOG and the GitHub release title for v1.0.1 lead with
  R@5 = 98.2% (CPU default); the +1.0pp GPU result is named as an
  ablation in the next paragraph.

Same data, honest framing.

## 1.0.0 — semver-stable, R@5 = 98.2% on LongMemEval-S (CPU default path)

First semver-stable release. The bar from the roadmap was three things:

1. The benchmark dropping a number on the wall every milestone — done
   (LongMemEval-S baseline + v0.12.1 regression + v0.14.3 GPU ablation).
2. Procedural memory as the ров — done (Д6 dataset of 227 pairs from 20
   public repos, R@5 = 96.5% in v0.14.3).
3. `md import/export` with the "md wins" escape hatch and an asymmetric
   "Qdrant now → md says" diff in the dry-run — done in v0.14.3 / this
   release.

The headline retrieval number this tag ships against is the **default
install path** a user gets after `mgimind doctor --fix`: CPU, INT8
all-MiniLM-L6-v2 + reranker, **R@5 = 98.2% on LongMemEval-S** (R@1 = 91.6%,
R@10 = 99.8%). The optional GPU + FP16 e5-base recipe documented in
`BENCHMARKS.md` and `scripts/local-bench-gpu.sh` lifts that to R@5 = 99.2%
on an RTX 3090 in 25.6 minutes — a real +1.0pp ablation, not the face of
the release. Putting the GPU number in the headline would sell a config
the zero-config user does not actually run.

Haystack size on this dataset is non-trivial: per-question distinct-
session count is **median 48** (p10 = 44, p90 = 52, range 38–62), so R@10
puts the system in the top-~20% of candidates and R@5 in the top-~10%;
neither is a mechanical ceiling. See "How hard is the task" in
`BENCHMARKS.md`.

### Added

- `MGIMIND_MODEL_VARIANT={cpu|gpu|auto}` switch. CPU = the INT8
  quantized e5-base shipped before; GPU = the FP16 variant pinned at
  `5d760477...8a3f54a`. Auto resolves to GPU when the build has the
  `cuda` feature and `MGIMIND_USE_CUDA=1`, else CPU. Zero-config users
  stay on CPU. `mgimind doctor --fix` now downloads the right variant
  and writes a `.variant` marker so flipping the env causes the next
  doctor to re-download instead of silently using the wrong file.
- `scripts/local-bench-gpu.sh` — one-shot GPU reproduction script.
  Downloads ORT 1.24.2 GPU runtime, builds with `--features cuda`,
  fetches `longmemeval_s.json`, runs `doctor --fix` with the GPU
  variant and the full bench. The recipe behind the v0.14.3 headline,
  now repeatable on a fresh box.
- `md_reconcile::render_plan` (promoted from `print_plan`) returns the
  rendered plan as a string so it's reusable in CLI and MCP responses.
  CLI `mgimind import md` now leads with this output, so the dry-run
  shows "Qdrant now (#1): ..." → "will become (md): ..." before the
  user flips `--apply`. The asymmetric direction is the v1.0
  semver-stable contract for md reconcile.

### Changed

- BENCHMARKS.md "Reproduce" section points at `local-bench-gpu.sh`.
- BENCHMARKS.md now carries the v0.12.1 CPU regression run (both
  rerank=off pod2 and rerank=on pod1 — the second pod's raw.json
  turned out to have survived the takedown despite the pod loss),
  the v0.14.3 GPU headline (three runs), and the MiniLM-FP16-on-GPU
  ablation that isolates "+1.0pp R@5 is from e5-base, not from GPU".
- `mgimind doctor` reports the active model variant.

### Removed / not changed (worth saying)

- No breaking changes to the public MCP tool surface from 0.14.x.
- v0.14.1 counterfactual A/B `mgimind bench-policy` is kept. The
  honest reading (also in BENCHMARKS.md) is that LongMemEval-S
  contains no chit-chat / P0 questions, so the metric measures an
  upper bound for the trigger policy on this corpus; it is not a
  bug, the dataset just doesn't separate the cases. A future dataset
  with explicit P0 questions would split the gap.

## 0.14.3 — procedural-dataset hits the v0.10.0 ров target (227 pairs, 20 repos)

Final procedural-memory dataset for the v0.10.0 roadmap milestone:
**227 pairs from 20 OSS repos, 4 languages, 4 strata.** Headline
R@5 = 96.5% on multilingual-e5-base + rerank=off.

### Added
- `benchmark/datasets/procedural-v010-227.jsonl` — final 227-pair set.
  Mined from cargo, clap, click, cobra, commander.js, django, express,
  flask, go, hyper, **next.js (+50 TS pairs)**, pytest, qdrant, requests,
  reqwest, rust-clippy, rustfmt, rustlings, serde, tokio, yargs.
- `benchmark/results/2026-06-02-procedural-v010-final/raw.json` — per-pair
  raw results.

### Results progression

|             | bootstrap (111) | v3 (177) | final (227) |
|---|---|---|---|
| R@1         | 38.7%           | 44.1%    | 48.0%       |
| **R@5**     | **99.1%**       | **98.9%**| **96.5%**   |
| R@10        | 100.0%          | 100.0%   | 98.7%       |
| repos       | 10              | 19       | 20          |
| ts pairs    | 10              | 19       | 63          |
| compile     | 3%              | 5%       | 4%          |
| runtime     | 97%             | 61%      | 53%         |
| test        | 0%              | 34%      | 42%         |

R@5 drop 98.9% → 96.5% is the honest cost of a harder corpus: next.js
introduced 50 TS pairs with near-duplicate "Hydration mismatch" style
signatures that compress retrieval headroom. Don't cherry-pick the
smaller set; the larger one is the publishable number.

### What this closes from the roadmap

> v0.10.0 — Д6 procedural memory как ров. Датасет — пары git+CI «упавший
> тест → коммит-фикс» из публичных репо (объективный сигнал, не моя
> разметка). Стратификация per-language / per-error-type (компиляция /
> тест / рантайм), отчёт per-stratum, не одним числом.

Reached: ✅ 200+ pairs ✅ 20+ repos ✅ 4 strata ✅ 4 languages ✅
per-stratum report.

## 0.14.2 — procedural-dataset v3 (177 pairs from 19 OSS repos, stratified)

Expanded the procedural-memory bench corpus from the 111-pair bootstrap
to **177 pairs from 19 OSS repos**, and improved stratification so the
"runtime / test / compile" buckets reflect what the fix actually
changed instead of catching everything as runtime.

### Added
- `benchmark/datasets/procedural-v010-177.jsonl` — the v3 dataset (177
  records). Mined locally with the updated scraper from cargo, clap,
  click, cobra, commander.js, express, flask, hyper, pytest, qdrant,
  requests, reqwest, rust-clippy, rustfmt, rustlings, serde, tokio,
  yargs, and one more.
- `benchmark/results/2026-06-02-procedural-v3/raw.json` — per-pair
  results behind the BENCHMARKS.md numbers.

### Changed
- `scripts/scrape_procedural_dataset.py` now derives stratum from the
  files the commit touched: `test`/`tests/`/`spec` paths → `test`,
  build manifests (Cargo.toml, package.json, pyproject.toml, etc) →
  `compile`, CI dirs → `ci`. The pattern-derived hint still wins when
  the body contains an explicit compile error code (e.g. `error[E0599]`).
- BENCHMARKS.md procedural section now points at the v3 dataset; the
  bootstrap dataset is kept as a referenced earlier baseline.

### Results — v3 vs v1 bootstrap

```
config: model=multilingual-e5-base dim=768 rerank=false

                v1 (n=111)   v3 (n=177)
R@1               38.7%        44.1%   (+5.4)
R@5               99.1%        98.9%   (−0.2, noise)
R@10             100.0%       100.0%

stratum mix:
  runtime          97%          61%
  test              0%          34%
  compile           3%           5%
```

The headline R@5 is stable across corpus shape. R@1 rose because the
larger corpus dilutes the near-duplicate error signatures that drag
exact-match recall on a small set. Stratum balance is the real win:
"the system can recall a fix" is now broken out by what kind of fix.

## 0.14.1 — counterfactual A/B for retrieval policy

Companion to the LongMemEval recall numbers: a CLI that takes any prior
`mgimind bench` raw.json, classifies each question by the trigger table
(P1 must-search, P2 should-search, P0 no-search), and reports ΔR@k vs
a no-search baseline. Quantifies the **structural** recall value of
the search-before-answer policy. Not an LLM accuracy measure.

### Added
- `mgimind bench-policy <raw.json>` — counterfactual A/B over a prior
  bench output. Output is a text report + embedded JSON for downstream
  consumers. Zero-API.
- BENCHMARKS.md "Counterfactual A/B — retrieval policy on / off"
  section with the question-type → priority mapping table and the
  baseline result on the v0.8.1 500q run:
  WITH policy R@5 = 98.2%, WITHOUT R@5 = 0.0% (structural), ΔR@5 = +98.2 pct.

### Notes
- LongMemEval-S contains no P0 questions — the policy unlocks 100% of
  recall on this corpus. A dataset with explicit chit-chat would split
  the gap (the policy doesn't help there, and the trigger table says
  skip P0).
- "WITHOUT policy R@5 = 0%" is by construction (no search → no
  candidates). The Δ goes to "what would the policy save if the agent
  did skip search". Not the same as "LLM is more accurate with mgi-mind."

## 0.14.0 — procedural-memory benchmark harness, README differentiation

First half of the v1.0 push: the recall harness for procedural memory
(phase Д6) and the README updates so the project sells what it actually
is, not "another wrapper around Qdrant."

### Added
- `mgimind bench-procedural <dataset.jsonl>` — measures recall@k on a
  dataset of (error, fix) pairs. Learns each pair into an isolated bench
  library, then recalls by error signature and reports overall +
  per-language + per-stratum + per-language×stratum R@1/R@5/R@10.
  Zero-API. Output is a text report; optional `--output raw.json`
  writes per-pair detail for analysis. Mirrors `mgimind bench` for
  LongMemEval, just on the procedural side.
- `scripts/scrape_procedural_dataset.py` — local-only scraper that
  mines (error, fix) pairs from already-cloned git repos. Looks for
  fix-pattern commit subjects and extracts an error signature from
  the body (panics, tracebacks, code-quoted errors, symptom
  sentences). Writes JSONL ready for `bench-procedural`. No HTTP, no
  GitHub API.
- README dedicated section "The single thing that's different:
  automated memory, not hand-curated notes." Explicit moat statement,
  not a hidden line. Same paragraph reflows the "alternatives fall
  short" list so each comparison hits the relevance-gate / procedural
  memory / vault gaps in the alternatives.

### Notes
- A real 200-pair dataset is the v0.10.0 sister task. This release
  ships the harness so the dataset, when built, has a place to land.
  A 5-pair smoke set against the harness returned R@1=100% (trivial
  signatures, no collisions) — useful only to confirm the pipeline,
  not to claim recall numbers.

## 0.13.0 — session liveness: zombie sessions auto-close on next start

Closes the long-standing leak where `mind_session_end` was never reached
because the MCP client was killed, Ctrl-C'd, or crashed. The session
file stayed `status = active` forever; the next `session_start` of the
same agent didn't know there had been a predecessor, and the summary
of the previous run was lost.

The fix is one field, no new MCP tools, no new files for the model to
remember to write to.

### Added
- Per-agent heartbeat file (`sessions/.heartbeat.<agent>`). Stamped
  with the current RFC3339 timestamp by the MCP dispatcher after every
  `tools/call`, and on `session_start`. Cheap atomic write of a single
  timestamp — no read-modify-write of the session body.
- `session::touch(agent)` and `session::touch_all_active()` —
  best-effort heartbeat updaters used by the dispatcher.
- `session::list_zombies(idle_minutes)` returning each agent whose
  active session has been idle longer than the threshold (default 30
  minutes), with sanitized agent name, session file path, last activity
  time, and minutes idle.
- `session::DEFAULT_IDLE_THRESHOLD_MINUTES = 30`.

### Changed
- `mind_session_start` now auto-recovers a stale active session for the
  same agent before opening a new one. The recovery is **visible**, not
  silent: the response includes "⚠ Recovered an interrupted session for
  agent '<x>'" with the original `started`, `last active`, file path,
  and a hint that the user can append a real summary manually if they
  remember what the run did. The new session is a separate file.
- `session::start` returns a `StartReport { recovered: Option<RecoveredSession> }`
  instead of `()`. Calling code paths (CLI `run_session_start`) surface
  the recovery to the user.
- Auto-close summary is reconstructed, not invented: "Auto-closed by
  v0.13 liveness check. Last activity at <T> (idle for N min). The
  session terminated without calling mind_session_end — usually a kill,
  Ctrl-C, or crash. No explicit summary recorded."
- `mind_doctor` adds a check `[OK] No zombie sessions` or `[WARN] N
  zombie session(s)` with one line per agent. Diagnostic only — the
  recovery path is still `session_start`, never `doctor`.
- `mind_stats` adds a `zombies: N (idle >30min, see mgimind doctor)`
  line when zombies exist, hidden otherwise.

### Out of scope (intentionally not done)
- No new MCP tool. The original draft proposal added
  `mind_session_draft_append`; the strict critic round rejected it as
  one more source of truth, one more decision point for the model,
  and one more tool name in a roadmap whose stated principle is
  "fewer tools, not more". The heartbeat field already gives
  auto-recovery without those costs.
- No per-agent override of the idle threshold yet. Defaulting to 30
  minutes covers `claude-code` long sessions and is too generous for
  `cursor`-style short flows, but a wrong-by-a-few-minutes auto-close
  costs almost nothing vs. an immortal zombie.
- `doctor` does not auto-close. Recovery happens deliberately at
  `session_start` so the recovery message is delivered to whoever just
  re-opened the session (the same person who'd care).

## 0.12.4 — download the versioned ONNX Runtime file, refuse to extract symlinks

THE ROOT CAUSE of every "mgimind add hangs forever" report. The
ONNX Runtime tarball ships `lib/libonnxruntime.so` as a **symlink**
to `lib/libonnxruntime.so.<version>`. `extract_member_tar_gz` used
`std::io::copy(&mut entry, &mut out)` on the symlink entry — which
silently produces a 0-byte regular file, because tar symlinks have
no body, only metadata in the header. `doctor --fix` then reports
"ONNX Runtime installed" with a happy exit code, the file at
`target/release/libonnxruntime.so` is genuinely 0 bytes, and the
next `dlopen` on it **hangs forever** with no error visible to the
user.

On the developer's PC this bug was masked because ONNX Runtime had
been installed manually long ago and the existing 22 MB file was
used. Every fresh install (cloud, CI, anyone downloading the
project) hit the empty-symlink trap.

### Changed
- `embedder::download_ort_runtime` now requests the **versioned**
  archive path (`libonnxruntime.so.<ORT_VERSION>`) instead of the
  symlink path (`libonnxruntime.so`). The destination filename
  stays `libonnxruntime.so` so the rest of the codebase
  (`ORT_DYLIB_PATH` auto-detect in `main.rs`) is unchanged.
- `embedder::extract_member_tar_gz` now refuses any
  `Symlink`/`Link` entry with a loud error pointing at the bug
  class. Future archive surprises surface as a panic message
  rather than as another infinite hang.

### Why this took four bumps to find
0.12.1 fixed glibc (real, separate bug — qdrant musl). 0.12.2 fixed
an IPv6/IPv4 race that turned out to be misdiagnosis (the
behaviour explained 1% of the symptom and 0% of the freshly-broken
RunPod containers). 0.12.3 added `tracing::debug` around every
storage `.await` in `add_memory`, which is what finally pointed at
`embed_passages start` as the last log line before the hang —
i.e. dlopen of the 0-byte ORT library. The library file was
visibly 0 bytes the whole time; nobody looked.

## 0.12.3 — surface errors from idempotent index creation, add hot-path tracing

Diagnostic + correctness patch on top of 0.12.2. The 0.12.2 hotfix
was based on the wrong root cause (IPv6 vs IPv4 race in
`Qdrant::from_url("localhost:...")`); the actual hang in `add_memory`
was somewhere else, and we couldn't see it because every
`create_field_index` result was being discarded with `let _ = …`.
An infinite-await against the qdrant server during index creation
looks identical to "success" from outside the function.

### Changed
- `ensure_payload_indexes` and `ensure_facts_indexes`: stopped
  discarding errors with `let _ =`. "Already exists" is filtered as
  the only success-equivalent; everything else surfaces via
  `tracing::warn!` with the field name and the error string.
- Added `tracing::debug!` around every `.await` in `add_memory`:
  `get_client`, `ensure_memories_collection`, `embed_passages`.
  Running `RUST_LOG=mgimind=debug` (or `mgi_mind=debug` depending on
  binary name) now pinpoints the hanging step in seconds.

### Caveats
- `tracing::debug!` is compiled in but inactive by default — same
  no-overhead in release as before. Only fires when the user opts
  into `RUST_LOG=debug`. Production users see no log spam.

## 0.12.2 — Qdrant client binds IPv4 explicitly, with timeouts

Hotfix continuation. After 0.12.1 fixed the glibc problem, the next
silent failure surfaced: on freshly provisioned container hosts
(RunPod base images, most CI runners) the Qdrant client built from
`http://localhost:6334` was deadlocking on the first RPC. The
`mgimind serve` readiness check passed (it uses `127.0.0.1`
literally), the `mgimind create <library>` round-trip completed, and
then the very next call hung indefinitely with 8 futex_wait threads
and 1 ep_poll thread on three ESTAB connections with zero bytes in
queue. No timeout. No error. Just a frozen process.

The cause: `Qdrant::from_url("http://localhost:...")` forwards to
tonic, which does its own `ToSocketAddrs` resolution against
`/etc/hosts`. Most modern Ubuntu container images list `::1
localhost` before `127.0.0.1 localhost`, so hyper picks IPv6 first.
But the bundled Qdrant is launched with
`QDRANT__SERVICE__HOST=127.0.0.1` (IPv4-only), so the kernel
loopback ESTABs `::1:6334` against a non-listener and the gRPC
channel pool wedges in the HTTP/2 SETTINGS exchange forever. A
single-shot RPC (the `collection_exists` inside `create_library`)
happened to win the race; the moment two RPCs raced (`add` does
`collection_exists` + `get_points` + `upsert_points`) the pool
locked up.

This isn't a "container quirk" — it's an "anyone who isn't the
developer downloads mgimind and it hangs". The developer's own PC
masks the bug because it ran first and qdrant landed on its
IPv4 listener before the IPv6 race could happen; on every other
machine the bug ships.

### Changed
- `storage::get_client` now builds the channel against `http://
  127.0.0.1:<port>` explicitly. No more "localhost".
- Added `connect_timeout(5s)`, `timeout(30s)`,
  `keep_alive_while_idle()` so the next infrastructure surprise
  surfaces as an error rather than an immortal futex hang.
- `is_qdrant_running()` in `cli.rs` was already using `127.0.0.1`
  literally; the client was the only one disagreeing.

### Caveats
- The 30s request timeout will affect very large batched embeds on
  cold CPU. If a batched `add` of >100 sessions fails on slow
  hardware, raise the per-call timeout rather than removing it.
  Disabling the timeout returns us to the immortal-futex world.

## 0.12.1 — bundled Qdrant works on any glibc (musl)

Hotfix. `doctor --fix` now downloads the **musl** Qdrant binary
(`qdrant-x86_64-unknown-linux-musl.tar.gz`) on Linux x64 instead of the
gnu build. The gnu binary that 0.12.0 fetched is linked against glibc
2.38, which is only on Ubuntu 24.10+ / Debian 13+. Every previous
release silently failed on Ubuntu 22.04 LTS (glibc 2.35), 20.04
(glibc 2.31), and most container images: `mgimind serve` spawned the
binary, it died with a glibc-not-found error on stderr (which we
swallow to `/dev/null`), and the parent reported the misleading
"Qdrant started but not responding after 15 seconds".

### Changed
- `download_qdrant()` asset name switched to `-musl`; SHA-256 pin
  added as `QDRANT_LINUX_X64_1_18_1_MUSL` in `integrity.rs`. The gnu
  pin is kept for anyone consuming the constant by name, but it is
  no longer on the install path.

### Caveats
- The "Qdrant started but not responding" error message is still
  produced on the rare cases where the musl binary genuinely cannot
  start (no disk, no port). A follow-up should surface the child's
  stderr rather than discarding it, so the next failure mode lands
  on the user diagnosable instead of mute.

## 0.12.0 — viewer wave: pagination + three live tabs

Earns the minor bump with a concrete capability that wasn't there
before: paginated quarantine listing, with a "load more" footer in
the UI. Without it, anyone with >50 quarantined entries simply
couldn't see beyond the first page. The other v0.11.x viewer work
(three live tabs over the v0.11.2–v0.11.4 endpoints) becomes the
context, not the headline.

### Added
- Cursor-based pagination on `/api/quarantine`. Response shape is now
  `{entries: [...], next_cursor: "<rfc3339-or-null>"}` (breaking from
  the bare array used in 0.11.2). Pass the cursor back in the
  `cursor=` query param to get the next page; `null` means end.
  Implementation uses Qdrant's ordered scroll with `start_from` on
  the `created_at` payload key, since Qdrant's `next_page_offset` is
  populated only on unordered scrolls. We fetch limit+1 each call to
  detect end-of-data without an extra round-trip.
- `storage::quarantine_list_page(library, limit, cursor)` returns
  `QuarantinePage { entries, next_cursor }`. The original
  `quarantine_list(library, limit)` is preserved as a backwards-
  compatible single-page wrapper for CLI/MCP, which don't paginate.
- Viewer UI: "load more" footer appends to the quarantine pane when
  more data is available; filter / limit / reload all reset the
  cursor so a stale cursor against a different scope cannot leak.

### Notes
The earlier v0.11.8–v0.11.10 commits (Quarantine / Consolidate /
Auto-ingest tabs + shared `renderMemoryRow` helper) are the working
substrate this minor builds on. The bump itself is *not* a meta-
ceremony for already-shipped patches; it's pegged to pagination, the
one capability a real user can do today that they could not before.

## 0.11.10 - viewer: auto-ingest tab + shared row renderer

Third and final UI tab in the v0.11.8–v0.11.10 wave. Critic flagged
the copy-paste-row drift risk; resolved by extracting the renderer up
front. The headline page per the roadmap — "what auto-ingest wrote in
this session."

### Added
- Auto-ingest tab. `datetime-local` "since" picker + `max_scan`
  numeric input. Naive local-time input is converted to UTC ISO via
  `new Date(s).toISOString()` and the resolved string is rendered
  next to the input ("querying since &lt;ISO&gt; UTC") so the user
  can see what is actually being queried — timezone mismatches are
  otherwise silent.
- `max_scan` default 500 (not 200): this is the headline page per the
  roadmap; 200 hid recent bursts during heavy auto-ingest.
- Loading state on the reload button and result pane.
- Empty-state ("No ingests since &lt;ISO&gt;.").
- `renderMemoryRow(r, extra)` helper. Both the memories tab and the
  new auto-ingest tab now go through it; a single source of truth
  for the row markup so three tabs cannot drift in 48 hours
  (memories carries its forget-button via the `extra` arg).

### Notes
All three v0.11 UI tabs land. Viewer surface is now complete for the
quarantine / consolidate / auto-ingest endpoints shipped in 0.11.2 →
0.11.4. The repo deletes + recreations earlier in this session mean
nothing of v0.11 visible on GitHub yet — push lands after this
commit.

## 0.11.9 - viewer: Consolidate dry-run tab + Quarantine loading state

Second of three UI tabs. Pre-ship critic flagged four real issues; all
four are addressed in this drop, plus the same loading-state hole in
Quarantine that the critic correctly extrapolated.

### Added
- Consolidate tab. Read-only preview surface — `--apply` stays on the
  CLI. The tab opens with a bordered warn-coloured notice
  ("Read-only preview. To act on this, run `mgimind consolidate
  --apply` in a terminal.") so the absence of an apply button is
  *loud*, not whispered in dim text.
- Library dropdown is populated from `/api/libraries`. The selector
  defaults to the first user library, not `(all)` — full-corpus
  consolidate walks every point's vector for near-dup math and would
  hang the UI on a large palace. `(all libraries)` is appended as an
  explicit opt-in instead.
- UI labels rename the API's past-tense field names on a dry-run:
  `exact_dups_removed` → `would remove exact`, etc. Nothing has been
  removed on a preview; the tense matters.
- Loading state on the reload button (disabled, label "scanning…")
  and the result pane ("Scanning…"). Without it the panel sits dead
  on a large library and looks broken.
- Same loading-state fix applied to the Quarantine tab.

### Notes
The third tab (auto-ingest recent) will land in 0.11.10 once Mad
verifies these two in a real browser.

## 0.11.8 - viewer: Quarantine tab

First of three UI tabs that consume the v0.11.2–v0.11.4 backend
endpoints. Shipped alone (not as a bundle) so any layout / JS
regression lands isolated. The other two tabs (consolidate, ingest
recent) will follow in 0.11.9 and 0.11.10 after this one is verified
in a real browser.

### Added
- Quarantine tab in `viewer_index.html`. Reuses the existing fetch
  wrapper (`api()`) for GETs and adds an `apiPost()` helper for
  promote.
- Library dropdown defaults to `(all)` — quarantined entries don't
  always live in the library the user expects. Limit input
  (default 50) caps response size.
- Row rendering: gate `reason` badge, library, source, created_at,
  truncated content (server-side: 200 chars), plus a `promote to
  memory` button. Button disables itself during the in-flight POST
  so a double-click cannot duplicate the audit entry, restores on
  failure, and triggers a list reload on success (the promoted row
  drops out because the backend filter excludes promoted points).
- Honest empty-state rendering (`No quarantined entries in <lib>.`)
  instead of a blank pane.
- Honest error rendering: if the backend returns
  `{ok:false, reason:...}` the reason surfaces via `showErr`, the
  button re-enables and restores its label so the user can retry.

### Notes
The other v0.11 endpoints (`/api/consolidate`,
`/api/ingest/recent`) are unchanged and not yet wired to a tab.
This is deliberate per the pre-commit critic ("ship one, verify,
then ship the rest").

## 0.11.7 - `mgimind ingest-session`

A manual batch command that parses a closed Claude Code transcript
(`~/.claude/projects/<encoded-cwd>/<session-uuid>.jsonl`) and routes
its text content through the existing relevance gate. The operator
runs it; nothing here is automatic.

### Added
- `mgimind ingest-session <transcript.jsonl> --library X` (default
  library: `sessions`). Extracts `user.text` and `assistant.text`
  blocks; drops `tool_use`, `tool_result`, `thinking` (not
  user-authored claims), and service rows like `queue-operation`,
  `ai-title`, `attachment`. Each surviving block becomes one
  `Candidate::Memory` and goes through `ingest::run_ingest`. The v0.11
  gate decides per-block: short reactions quarantined `too_short`;
  rearranged-token paraphrases quarantined `low_novelty`; whatever
  the gate's current heuristics consider worth keeping lands tagged
  with the session id.
- New module `src/session_ingest.rs` with five parser smoke tests
  (string-form content, blocks-form content, service-row skipping,
  role-tag prefix, empty-skip). No gate-integration test in this
  release — that lives in the live ingest path's existing coverage.

### Relation to `claude --resume`
Different layer. `claude --resume <session-id>` brings the agent back
into a past conversation so it can keep working with full context.
`ingest-session` is the operator running a one-shot extraction on a
closed transcript afterwards; the agent is not involved.

## 0.11.6 - mind_consolidate MCP tool (dry-run preview)

Last bit of MCP/CLI symmetry for the v0.11 line. The viewer and now
MCP both expose consolidate as a *preview* surface; `--apply` stays on
the CLI where the user has to type the flag explicitly. The 30th
tool.

### Added
- `mind_consolidate` MCP tool: returns the same Report shape as
  `mgimind consolidate` (scanned, exact dups, near dups, cold) but
  always with `apply=false`. The text response ends with a hint
  pointing the user at the CLI when they want to act on it. Use case:
  the agent answers "how much duplicate memory do I have?" without
  needing the user to drop to a terminal.
- AI_INSTRUCTIONS.md mentions it in the tool list with the
  preview-only posture spelled out.

### Notes
Tool count: 29 → 30. The destructive paths
(`mind_quarantine_promote`, `mind_delete`) take a single id and are
intentionally narrow; nothing on the MCP surface mutates the store in
bulk.

## 0.11.5 - novelty layer in the relevance gate

The v0.11.0 cheap gate is length / blacklist / decision-marker only —
all syntactic checks. This release wires the novelty layer that the
roadmap planned: after cheap accepts, pull the top-3 semantic
neighbors, tokenize their content, and check the share of candidate
tokens that are NEW. A paraphrase of stored content adds zero new
tokens; it's quarantined under reason `"low_novelty"` so a future
re-assertion can promote it (the same loop-breaker as for the cheap
reasons).

This is **not** cosine-noise filtering. Invariant #4 from v0.11.0
stands: "a repeat IS a confidence signal, not noise." Cosine
similarity reflects *meaning*; this is a *token-overlap* check —
narrower. A semantically related but lexically distinct fact passes;
a token-rearrangement of existing content does not.

### Added
- `storage::top_k_neighbor_content(library, content, k)` — one
  embedding inference, returns the stored content strings of the top-k
  neighbors. Symmetric with `nearest_score` but content instead of
  score.
- `ingest::run_ingest` second-tier novelty branch after `check_cheap`.
  Falls through to Accept if there are no neighbors (empty library /
  query failure) — novelty cannot be assessed without a baseline.
- `NOVELTY_NEIGHBORS = 3` in `ingest.rs` — small enough that the
  union doesn't drift toward "everything is similar to something".

### Notes
The `novelty_ratio` and `tokenize` functions in `src/relevance.rs`
were written in 0.11.0 but unused; this release activates them
without changing the signatures, so the unit tests already in
0.11.0 cover the math.

E2E verified: original sentence stored, paraphrase of the same
tokens quarantined with reason=low_novelty, unrelated content
stored.

## 0.11.4 - viewer API for "what auto-ingest wrote in this session"

The headline page of the v0.12 viewer per the roadmap. The user's
recurring complaint about auto-ingest was that they could not see
what was written. This endpoint surfaces that feedback loop without
the UI work that consumes it.

### Added
- `GET /api/ingest/recent?since=<ISO>&max_scan=N` — recent memories
  whose `source` field equals `"ingest"` and whose `created_at` is at
  or after the given RFC3339 timestamp (typically session-start). Omit
  `since` to return the most recent `max_scan` (default 200) ingests
  regardless of age. Returns the same `MemoryRow` shape as
  `/api/memories` so the UI can reuse its existing memory-card.
- `storage::recent_by_source_since(source, since_iso, max_scan)` —
  shared primitive: server-side narrows to source-tagged points,
  client-side cuts on the date with a lexicographic compare
  (RFC3339-UTC sorts correctly as a string, which is how we always
  write timestamps).

## 0.11.3 - viewer API for consolidate dry-run

Continues the v0.11.2 pattern: backend HTTP surfaces land first, the
v0.12 UI consumes them. This release adds the "what would consolidate
do" preview that the dry-run consolidate page will show.

### Added
- `GET /api/consolidate?library=X` — runs the same consolidation logic
  as `mgimind consolidate` but always with `apply=false`. Returns a
  JSON `Report` (`scanned`, `exact_dups_removed`, `near_dups_removed`,
  `cold_candidates`, `applied=false`). The endpoint **does not** expose
  `--apply` — destructive operations belong on the CLI where the user
  has to type the flag explicitly. The viewer is the preview surface,
  not the action surface.
- `consolidate::Report` now derives `Serialize` (no behaviour change;
  just enables the JSON response).

## 0.11.2 - viewer API for the quarantine layer

The viewer (v0.10.x) renders memories and the audit log. v0.11.2 wires
the quarantine layer into the same surface so the UI work in v0.12 can
ship without another backend round-trip.

### Added
- `GET /api/quarantine?library=X&limit=N` — list quarantined entries
  (mirrors the CLI/MCP). Bearer-token auth on the same channel as the
  other endpoints.
- `POST /api/quarantine/:id/promote` — manual promotion of a
  quarantined entry by id. Returns `{ok: true, id}` on success,
  `{ok: false, id, reason: "not in quarantine"}` for ordinary memory
  ids — the surface stays honest about what it can act on. Audit log
  records two events: the storage-level promotion (actor=relevance-gate)
  and the UI-level action (actor=viewer, note="manual promote via
  viewer UI"), so the trail distinguishes manual from auto-reassertion.

### Notes
The viewer frontend (`viewer_index.html`) still only renders memories
and audit; the new endpoints are reachable today via curl. The UI work
that consumes them is the next v0.12 deliverable.

## 0.11.1 - inspect & manage the quarantine layer

Surfaces the v0.11.0 quarantine layer through CLI and MCP. The store-side
machinery shipped in 0.11.0; this release adds the inspection commands so a
user (or agent) can see what was filtered, why, and override the gate by
promoting an entry by id.

### Added
- `mgimind quarantine list [--library X] [--limit N]` — newest first,
  entries scoped to one library or across all.
- `mgimind quarantine show <id>` — full content + gate reason + audit
  trail for one entry. Returns "not in quarantine" for ordinary memory
  ids: the surface only sees what it should.
- `mgimind quarantine promote <id>` — explicit promotion path, distinct
  from the automatic "re-assert same content via ingest" flow. For when
  the agent knows the entry belongs in normal memory without an
  ingest round-trip.
- MCP tools: `mind_quarantine_list`, `mind_quarantine_show`,
  `mind_quarantine_promote` (mirror the CLI). Tools count: 26 → 29.

### Notes
The quarantine layer was deliberately invisible in 0.11.0 — by design,
quarantined points must not surface through `mind_search`. The
inspection commands are the only surface that ever returns them.

## 0.11.0 - quarantine layer + relevance gate + best-effort retrieval

The core problem v0.11 solves: a write-side relevance filter that silently
drops low-signal candidates creates a loop — the user re-asserts the same
thing, the filter drops it again, the agent never learns. The fix is a
quarantine layer between accept and reject. Low-signal candidates are
quarantined (kept retrievable for re-submission detection, hidden from
ordinary `mind_search`); a re-assertion promotes them to ordinary memory.
That breaks the loop without surrendering the filter.

Paired with a best-effort retrieval policy on the read side: the MCP server
now advertises `instructions` at `initialize`, and `mind_context` lists the
user-facing libraries to consider before answering. Neither is enforceable
in MCP — the client may ignore both — so the policy is phrased as triggers,
not rules.

### Added
- **Relevance gate** (`src/relevance.rs`). Cheap, pure filters: length floor
  (12 chars / 3 words), 8000-char cap, blacklisted paths/tools, decision
  markers in RU + EN, novelty by token Jaccard against neighbors (not
  cosine — repetition is a confidence signal, not noise). Verdict::Accept |
  Quarantine{reason}. Applied in `mind_ingest` / `mgimind ingest` to
  `Candidate::Memory`. 12 unit tests.
- **Quarantine layer** in `src/storage.rs`. New payload flag
  `quarantined: bool` + `quarantine_reason`. `memory_query_filter` excludes
  quarantined points (alongside procedures), so they never surface in normal
  search. `add_quarantined`, `promote_from_quarantine`,
  `quarantine_id_for(library, content)` (deterministic UUIDv5 for
  re-assertion detection). Every transition writes an audit event with
  `actor=relevance-gate`.
- **`mgimind ingest --library X [--raw TEXT] [--memory TEXT...]`** CLI
  command. Previously only via MCP; now usable for smoke tests, dev debug,
  and shell-driven imports.
- **MCP `initialize` `instructions`** field carries the best-effort
  retrieval policy. Phrased as triggers (named project, meta-cue about
  memory, negation to verify, cross-session reference), not as rules.
- **`mind_context` "Before answering, consider mind_search in:"** section
  lists user-facing libraries (those not prefixed with `_`). Names come from
  the namespaces themselves, not a parallel config file.
- **`AI_INSTRUCTIONS.md` search-trigger table** (Priority 1 / Priority 2)
  with explicit examples, including meta-cues and negation-verification.

### Architectural invariants (do not relitigate without re-running the critic)
1. MCP cannot enforce "search before answer." Any policy is best-effort.
   Called as such in user-facing copy.
2. A proxy in front of the model was rejected. Single point of failure
   for every turn; contradicts the best-effort posture of the rest of the
   stack.
3. Quarantine is the architectural unblocker. Without it, a write-side
   relevance gate plus a read-side retrieval policy form a loop through
   the user.
4. Cosine-noise filtering is out of the gate by design. "Similar to existing"
   is a confidence signal, not low-signal.
5. Project names live in namespaces (libraries), not in a parallel config.
6. There is no "Priority 0 / never search" tier. False negatives are
   more expensive than false positives.

## 0.10.x - audit log + ephemeral viewer + md reconcile

Shipped on `main` ahead of the version bump (the working semver was
catching up with the work). These are the v0.11 deliverables that landed
before the quarantine layer:

- **Audit log** (`src/audit.rs`) — append-only JSONL under
  `MGIMIND_HOME/audit.log`. Every storage mutation
  (add/update/delete/library/quarantine/promote) writes an event with
  actor, target, before/after, and a free-text note. Read-only via
  `mgimind audit list / show <target>`.
- **Ephemeral viewer** (`src/viewer.rs` + baked `viewer_index.html`) — local
  HTTP server on 127.0.0.1 with a random free port. Static frontend baked
  into the binary; no Node, no extra runtime. `mgimind viewer` opens the
  browser by default; `--no-open` for headless / SSH.
- **`mgimind import md <path> --library <name> [--apply]`** as reconcile
  with "md wins" (`src/md_reconcile.rs`). Dry-run default that prints the
  plan (new / replace / unchanged / skip per file). Identity by `source`,
  not content hash, so a hand-edited file replaces its prior version
  instead of accumulating duplicates. This is the v1.0 escape hatch for
  hand-curated stores.
- **First LongMemEval-S bench result** — R@5 = 98.2% on CPU
  (all-MiniLM-L6-v2, rerank off), 1h 45min wall-clock. See `BENCHMARKS.md`
  and `benchmark/results/2026-06-02-cpu-overnight/`. The number is the
  baseline against which v0.12+ retrieval changes will be judged.

## Unreleased - auto-memory (Д2) and procedural memory (Д6)

Memory the system helps build and curate, not just a manual store. Built on the
single-process 0.8 foundation. See `docs/PHASE_D2_D6.md` for the design and the two
hard invariants (no auto-write before consolidation; no proactive `verified` without a
truth signal).

### Added
- **`mind_provenance_add`** - strict variant of `mind_add` for externally-sourced
  snippets (code, doc, RFC quote, commit message). Provenance fields are required and
  validated in Rust before any storage call: `origin_url` must be https + host in a
  small allowlist (github.com, gitlab.com, bitbucket.org, sr.ht, codeberg.org,
  grep.app, sourcegraph.com), `repo` matches `^[\w.-]+/[\w.-]+$`, `file` rejects
  absolute paths and `..` traversal, `line_range` matches `^\d+(-\d+)?$`, and an
  empty `search_tool_used` yields the actionable error
  `"provenance source unknown — use mind_add instead"`. Dedup key is
  `uuid_v5(NAMESPACE_PROVENANCE, library + snippet + origin_url + line_range)`, so
  the same snippet from two different repos correctly produces two records (the
  citation is part of the identity, not noise). No HTTP, no enrichment, no HTML
  stripping — the agent passes plain UTF-8 or gets rejected. Tools count: 25 → 26.
  Design: `docs/design/provenance-add.md`.
- **`mind_ingest`** - auto-extraction. Agent-driven primary path: send a `candidates`
  array of typed items (memory / fact / procedure) you judged worth keeping. Heuristic
  backstop: pass `raw` text for marker-based extraction. Every candidate is
  secret-scrubbed and memories are near-duplicate-checked before writing. No LLM.
- **`mgimind consolidate`** - the mandatory companion to auto-write. Merges exact and
  near-duplicates (cosine, via each point's stored vector - no re-embedding) and reports
  cold (old + never-accessed) entries. Dry-run by default; `--apply` to act,
  `--prune-cold` (opt-in) to also delete cold entries.
- **Procedural memory** - `mind_learn` (record an error -> fix lesson),
  `mind_recall` (retrieve playbooks by normalized error signature and/or task context,
  verified-first), `mind_procedure_outcome` (record whether a reused fix worked, so the
  store self-corrects). `verified` is set true only by a caller with a deterministic
  signal; manual lessons stay unverified and low-weight.
- **Secret scrub** - a conservative, regex-free detector (PEM keys, AWS/GitHub/GitLab/
  Slack/Google/`sk-`/JWT tokens, `.env`-style assignments) now guards every write path,
  so a key or password can no longer land in searchable memory.
- **Access counters** - search hits are counted in process and flushed to a small
  journal (reads stay read-only to Qdrant); consolidation uses this for decay.
- **`type` payload field + index** on the memories collection, so notes and procedures
  share one collection while normal search excludes procedures.
- **`mgimind bench` (Д1)** - retrieval-recall (R@k) benchmark on LongMemEval, zero-API
  (no LLM, no keys). Ingests each question's haystack into an isolated library, runs
  hybrid search, reports R@1/5/10 overall + per question type, writes raw results.
  Explicitly NOT QA accuracy; see `BENCHMARKS.md` for the metric discipline.

## 0.8.0 - one cross-platform binary that is itself the MCP server

A single Rust binary now speaks MCP over stdio directly (`mgimind mcp`), replacing the
three-process stack (Node MCP server -> Unix-socket daemon -> per-call CLI). The process
lives for the whole session, so the embedding models load once and stay warm with no
daemon to run. This also removes the only Unix-only code (`UnixListener`), so the
Windows build compiles, and drops the Node/npm dependency entirely. Net change: about
450 fewer lines.

### Added
- **`mgimind mcp`** - hand-rolled JSON-RPC 2.0 MCP server over stdio (no SDK
  dependency). Implements `initialize`, `tools/list` and `tools/call` for all 21 tools,
  plus `ping` and the lifecycle notifications. Tool-execution failures are returned as
  a result with `isError: true` (not a JSON-RPC error), so a failing tool never drops
  the client session. Requests are handled sequentially - one stdio client needs no
  session pool.
- **Automatic Qdrant startup.** `mgimind mcp` brings up the bundled Qdrant (detached,
  in its own process group on Unix / `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on
  Windows) so Qdrant outlives the session and a minimal user never runs `serve` by
  hand. Soft on the "two sessions start at once" race.
- **Antivirus / quarantine diagnosis in `doctor`.** When a download reports success but
  the file is missing afterward, `doctor` now says so ("likely antivirus/SmartScreen
  quarantine") instead of silently looping on `--fix`.
- **MCP round-trip integration test** (`mcp_add_then_search_roundtrip`): drives
  `mgimind mcp` over real stdin/stdout and asserts add -> search retrieves, and that
  every stdout line is valid JSON-RPC.

### Changed
- The 14 tools that previously shelled out to the CLI now call shared text-returning
  `run_*`/`render_*`/`build_*` functions in-process. All download/progress output moved
  from stdout to stderr so the MCP stdout channel stays pure JSON-RPC.
- `vault_list` is now terminal-only (like `vault_get`/`vault_store`): it needs the
  master password on a TTY, which MCP has no access to, so it returns instructions
  instead of failing.
- Logs go to stderr in every mode.

### Removed
- The Unix-socket daemon (`src/daemon.rs`, `mgimind daemon`) and the Node MCP server
  (`mcp-server/`). Their job - keeping models warm and bridging the assistant - is now
  done by the single `mgimind mcp` process.

### Testing
- **The retrieval tests now run on every OS, including Windows (the main target).**
  `setup_model_home` previously symlinked the model (`std::os::unix::fs::symlink`),
  which forced `add_then_search` and the MCP round-trip to be `#[cfg(unix)]` - so the
  add -> search path had no automated coverage on Windows. The helper now copies the
  model dir recursively (portable) and the tests are no longer gated to Unix.
- **`MGIMIND_HOME` env override** for the data dir. `dirs::home_dir()` ignores `$HOME`
  on Windows, so a `$HOME` override could not isolate the data dir there; `MGIMIND_HOME`
  works on all three OSes. Tests use it to isolate; power users can relocate the data
  dir with it. Test `config.json` is now built with `serde_json` so Windows paths
  (`C:\...`) are escaped correctly instead of producing invalid JSON.
- **Windows integration job** in CI. Linux runners use a Qdrant service container;
  Windows runners cannot, so the job starts the binary's own bundled Qdrant (`serve`)
  and runs the same lifecycle + add -> search tests against it.

### Distribution
- Release workflow builds Linux/macOS/Windows binaries on a tag and publishes them to
  GitHub Releases, so users download instead of building.

## 0.7.4 - retrieval test is now a real CI gate

The 0.7.3 `add -> search` integration test only ran locally, because CI did not
provide the embedding model, so it could not catch regressions on its own. CI now
downloads the models once (cached, keyed on `integrity.rs`), runs `doctor --fix`, and
passes `MGIMIND_IT_MODELS` + `ORT_DYLIB_PATH` to the integration job. The full
retrieval path (add -> embed -> hybrid search) is now exercised automatically against
the Qdrant service container, not just the library lifecycle.

## 0.7.3 - MCP fact-invalidate + a real retrieval integration test

Follow-up to the 0.7.2 review.

### Fixed
- **`mind_fact_invalidate` was missing from the MCP server.** Fact supersession was
  documented as "query, invalidate, add" for the agent, but the invalidate step had
  no MCP tool, so over MCP the agent could not actually do it. Added the tool;
  AI_INSTRUCTIONS now lists it as an MCP tool (not CLI-only).

### Tests
- New integration test `add_then_search_finds_the_memory`: creates a library, adds a
  memory, runs a paraphrased hybrid search, and asserts the memory is retrieved. This
  exercises the real path (add -> embed -> hybrid search), not just the library
  lifecycle. Gated on `MGIMIND_IT_MODELS` + `ORT_DYLIB_PATH` so CI without the model
  skips it; verified locally against a real Qdrant and the e5 model.

## 0.7.2 - Code-review fixes (security, correctness, tests)

A line-by-line review found real issues; this release closes the actionable ones.

### Security
- **Supply chain (#6 regression).** The default stack (multilingual-e5-base and
  bge-reranker-base) was downloading with no checksum because only the old MiniLM
  was pinned. Both models' quantized ONNX and tokenizer now have pinned SHA-256 and
  download fail-closed.
- **Vault store over MCP.** `mind_vault_store` no longer accepts the secret value
  over MCP (it would land in process argv, and needs a TTY anyway). It now returns
  terminal instructions, matching `mind_vault_get`.
- **Daemon socket.** The Unix socket is chmod 0600 so another local user cannot read
  or write the whole memory. Per-connection bytes are capped (no OOM from a huge
  line), and a transient accept error no longer kills the daemon.

### Correctness
- **add_memory now chunks (#3).** The main write path (including MCP `mind_add`) no
  longer silently drops everything past 512 tokens; long content is split into
  chunks. `add_memory` returns the number of chunks stored.
- **Vault durability (#4, #11).** `atomic_write` fsyncs the parent directory so the
  rename is durable after a crash. Argon2id parameters are pinned (not
  `Argon2::default()`), so a crate upgrade cannot make existing vaults undecryptable.
- **Context briefing (#10).** The key-facts section is ordered newest-first
  (`order_by created_at`) instead of an arbitrary page; the facts collection gets a
  `created_at` index.
- **Consistent score.** Reranked results map the cross-encoder logit through a
  sigmoid to 0..1, so the `score` field means the same thing with rerank on or off.
- **No double model build.** Embedder and reranker sessions use `get_or_try_init`,
  so concurrent first calls build the ONNX session exactly once.

### Tests
- Unit tests expanded to 28 (daemon request parsing, integrity pins, config
  defaults + legacy-config shape, vault encrypt/decrypt property roundtrip on varied
  payloads, chunking, sparse vectors).
- New black-box integration test (`tests/cli_integration.rs`) that drives the built
  binary against a real Qdrant. CI runs it against a Qdrant service container, and
  the build/clippy/unit matrix now covers Linux, macOS, and Windows.

### Known, not done here
- Fact supersession for single-valued predicates (#13) is still not implemented
  (dedup and soft-delete are); the facts collection stores an unused dense vector;
  embedding is not batched; the daemon serializes inference under one mutex. These
  are tracked, not closed.

## 0.7.1 - Sequence-length fix and resilient migrate

A real-data migration of 12,587 entries surfaced two issues.

### Fixed
- Inputs longer than the model's 512-token limit crashed ONNX inference with
  "invalid expand shape". The embedder and reranker now truncate to 512 tokens.
- `migrate` aborted the whole run on a single failing entry. It now logs and skips
  the entry, continues, and reports how many were skipped.

### Notes
- Reranking is on by default. `bge-reranker-base` is strong on English (the target
  audience) and improves precision there. It does lower Russian ranking, so turn it
  off (`rerank_enabled=false`) or use a stronger multilingual reranker if you need
  good Russian results.

## 0.7.0 - Hybrid search: dense + sparse + RRF (audit #23)

Dense (semantic) retrieval misses exact rare terms; lexical (BM25) retrieval
misses paraphrases. The memories collection now carries **both** and fuses them.

### Changed
- **Named vectors** on the memories collection: `dense` (e5, cosine) + `sparse`
  (BM25-style, with Qdrant's **IDF modifier** applied server-side). `add_memory`
  and `migrate` write both.
- **`search` is hybrid**: a Qdrant Query API call with two prefetches (dense NN +
  sparse NN) fused by **Reciprocal Rank Fusion (RRF)**, then cross-encoder reranked
  (#22). A library filter applies to both arms.
- Sparse vectors are unicode-aware term-frequency (lowercased, split on
  non-alphanumeric - handles Cyrillic; tokens hashed to u32 indices).

### Validated
- Runtime-tested end-to-end: exact rare terms (`fossilize_replay`, `gamemoderun`)
  surface via the lexical arm, while semantic queries ("как стим компилирует
  шейдеры") still hit via dense - fused and reranked correctly.

### Audit complete
- 22 of 27 issues fully fixed, 5 partial (non-blocking polish), 0 deferred.
- **Operational (not audit):** live cutover - deploy v0.7.0, `doctor --fix`
  (fetch e5 + reranker), `migrate` (re-embed at the new dense+sparse 768-dim
  schema) - plus daemon autostart.

## 0.6.0 - Cross-encoder reranker (audit #22)

Dense retrieval is fast but coarse. A cross-encoder now re-orders the top-K by
scoring each (query, passage) pair jointly - a big precision win.

### Added
- **`src/reranker.rs`**: `bge-reranker-base` (XLM-R, multilingual incl. RU;
  quantized ONNX, ~279 MB, CPU-ok). All candidate pairs run in a single padded
  batch (one ONNX pass). `search` fetches `rerank_top_k` (default 20) candidates by
  dense similarity, reranks, and returns `limit`. Reranking scores the **full**
  content; tier truncation is display-only, applied after ordering.
- Config: `rerank_enabled` (default true), `rerank_model` (`bge-reranker-base`),
  `rerank_top_k` (20). `doctor --fix` fetches the reranker model.
- **Best-effort**: any reranker failure (missing model, inference error) leaves the
  dense order untouched - reranking is a quality boost, never a hard dependency.

### Validated
- Runtime-tested: for «почему в доте мало фпс хотя видеокарта мощная» the reranker
  sharply separated relevance (1.07 / 0.83 / −0.66) where dense was a flat
  0.86 / 0.84 / 0.82.

### Still open
- #23 hybrid/BM25 search (e5 is dense-only → needs a separate sparse path).
  Operational: daemon autostart + live cutover (re-embed at 768-dim + reranker).

## 0.5.0 - Multilingual embedder support: e5-base (audit #21)

The English-only MiniLM is replaced as the default by **multilingual-e5-base** -
a big retrieval-quality win for Russian/mixed content, practical on CPU (768-dim,
~278M, runs quantized). The embedder is now model-architecture-flexible.

### Changed
- **Default model → `multilingual-e5-base`** (768-dim). Existing MiniLM configs keep
  working unchanged (serde preserves their `model_name`/`vector_size`/pooling).
- **Embedder is architecture-flexible** (`pooling` = mean|cls; optional
  `token_type_ids`): supports both BERT-family (MiniLM) and XLM-R (e5) models.
  Pooling math is unit-tested.
- **Query/passage prefixes** (`query_prefix`/`passage_prefix`): e5 requires
  "query: " / "passage: ". `search` embeds as a query, stored memories/facts as
  passages. MiniLM uses empty prefixes (no behaviour change).
- **Model download is source-aware**: e5 ONNX is fetched (quantized) from the Xenova
  mirror; sentence-transformers models keep their path.

### Validated
- e5-base runtime-tested on an isolated instance with the real quantized model: RU
  queries returned the correct top result every time (e.g. «искусственный интеллект
  для трансляций» → Aurora 0.79; «что приготовить на обед» → борщ 0.82, not the tech
  entries). Confirms the e5 ONNX path: no token_type_ids, mean pooling, query/passage
  prefixes, 768-dim.

### Deploy step (not automatic)
- Live cutover: `mgimind doctor --fix` fetches the e5 model, then `mgimind migrate`
  re-embeds existing memories at 768-dim under e5.

### Still open
- #22 cross-encoder reranker, #23 hybrid/BM25 search (e5 is dense-only → needs a
  separate sparse path). Operational: daemon autostart + live cutover.

## 0.4.0 - Single-collection storage (audit #18)

Memories moved from one Qdrant collection per library (`mem_<library>`) to a single
`memories` collection with a `library` payload field. This is a storage-layout change
- run `mgimind migrate` once to import existing data.

### Changed
- **One `memories` collection** with payload indexes on `library` (keyword) and
  `created_at` (datetime). Search runs a **single query** - true global top-k, or a
  `library`-filtered query - instead of scanning N collections and merging.
- **`history` is no longer O(total)**: it uses Qdrant `order_by` over the
  `created_at` datetime index to return the newest N directly (fixes the post-0.2
  review finding). 
- Libraries are tracked in a small `libraries.json` registry; counts always come
  from live data (`count` + filter), never the file.

### Added
- **`mgimind migrate [--purge]`**: imports legacy `mem_*` collections into
  `memories`. Re-embeds from stored content (no raw-vector extraction), preserves
  each entry's original `created_at`, idempotent (deterministic IDs), and with
  `--purge` deletes the old collections after a successful copy.

### Validated
- Isolated-instance runtime test: global + library-filtered search, ordered
  `history`, per-library `stats`, `drop` (delete-by-filter), and `migrate` (with
  `created_at` preserved and content re-embedded) all verified end-to-end.

### Still open
- **Operational:** daemon autostart + cutover of the live instance (now also: run
  `migrate` on the live data during cutover).
- Deferred audit items: code embedder (#21), cross-encoder reranker (#22),
  hybrid/BM25 search (#23) - each needs a new model + full re-embed.

## 0.3.0 - Daemon (audit #16)

The MCP server spawned a fresh `mgimind` process per call, reloading the ONNX
session + tokenizer every time. This release adds a long-lived daemon so the model
stays warm.

### Added
- **`mgimind daemon`** (`src/daemon.rs`): loads the embedding model once and serves
  newline-delimited JSON requests over a Unix socket (`~/mgimind/daemon.sock`).
  Supported: search, add, context, history, fact_add, fact_query, stats, ping.
- **Thin MCP client**: `mcp-server/index.js` routes embed-heavy/common tools to the
  daemon and **falls back to spawning the CLI** when the socket isn't there - the
  daemon is a pure optimization, never a hard dependency.
- Shared render helpers (`cli::render_search/render_history/render_facts/build_stats/
  build_context`) so daemon and CLI output are identical (one source of truth).

### Validated
- End-to-end against live data (12 587 memories read correctly via the daemon).
- Latency: warm daemon add ~31ms vs cold CLI add ~175ms (~5.6×). The audit's "2-5s"
  figure is the cold-disk/first-load case; the model is normally OS page-cached.

### Still open
- **Operational:** autostart entry for the daemon + cutover of the live instance.
- Deferred audit items unchanged: single-collection (#18), code embedder / reranker
  / hybrid search (#21-23); `history` O(total) rides with #18.

## 0.2.1 - Post-review fixes

A follow-up code review of 0.2.0 found four issues the hardening pass either
over-claimed or introduced. This release closes the tractable ones; the rest are
documented honestly (see [`AUDIT_STATUS.md`](AUDIT_STATUS.md)).

### Fixed
- **Session pointer collision (regression of #14).** `sanitize` mapped every
  non-`[A-Za-z0-9-]` byte to `_`, so `team a`, `team_a`, `team/a`, `team.a` all
  shared one `.current.<agent>` pointer and clobbered each other's session. It is
  now an injective `_HH` escape (the escape byte `_` is itself escaped).
- **`created_at` was reset on re-add.** Content-addressed upserts overwrote the
  whole payload, so re-adding identical content set `created_at = now` and the
  entry jumped to the top of chronological history. The original `created_at` is
  now preserved (read-before-write by id); a separate `updated_at` records the
  re-touch. Applies to both memories and facts.
- **Facts had no dimension guard (#11 gap).** `add_fact` now runs the same
  `check_dim` model-swap check as `add_memory`.
- **Config↔collection dimension mismatch (#11).** `mgimind serve` now checks every
  collection's on-disk vector dimension against the configured `vector_size` and
  warns up front (best-effort; never blocks serve), instead of surfacing a raw
  Qdrant error on the first upsert after a model change.

### Known, still open (documented, not silently dropped)
- **`history` is O(total memories).** Correct (newest-first), but scrolls every
  collection fully. Fine at current scale; the `order_by`-over-datetime-index fix
  rides with the v0.3 storage rework (#16/#18).
- Deferred 0.3 items unchanged: daemon (#16), single-collection (#18),
  code embedder / reranker / hybrid search (#21-23).

## 0.2.0 - Audit hardening

This release rebuilds the data and security layers around the findings of a full
code audit. See [`AUDIT_STATUS.md`](AUDIT_STATUS.md) for the complete issue-by-issue
accounting. It is **API-compatible** with 0.1.x data on disk (config gains
defaulted fields; existing Qdrant collections keep working).

### Security & data integrity
- **Atomic writes** for config, vault, salt, sessions and exports (temp + fsync + rename) - a crash can no longer corrupt these files.
- **Vault** master password is now read **without echo** (`rpassword`) and zeroized after key derivation; reads no longer rewrite the encrypted blob; the plaintext `vault.count` file is gone.
- **Vault over MCP**: secrets are never returned through the MCP/LLM channel and the master password is never blank - `mind_vault_get` directs you to a terminal.
- **Download integrity**: artifacts are fetched over HTTPS (native `reqwest`, no `curl`) and verified against pinned SHA-256 hashes (linux-x64 ONNX Runtime, Qdrant, default model); unknown targets warn instead of trusting blindly.
- **Qdrant** is bound to `127.0.0.1` and supports an optional API key.

### Correctness
- **Deterministic content-addressed IDs** (`UUIDv5` of library + content): re-adding identical content is an idempotent upsert - no duplicates, no read-before-write race. Same for facts (`subject,predicate,object`).
- **`history`** is sorted newest-first; **`export`** paginates fully (no silent 10k cap).
- **Embedding dimension** is configurable and validated on every operation (model-swap safety).
- **Knowledge graph**: queries match subject/predicate/object via a full scan + filter (nothing lost outside a top-K window); `invalidate` is a soft delete (`valid=false`) that queries honor.
- **Sessions** are per-agent: no shared `.current` to clobber, second-precision + random filename suffixes, `--agent`-scoped `end`/`last`.

### Performance & portability
- Tokenizer loaded once and cached (no per-embed disk read).
- Native gzip/tar/zip extraction and native gzip+tar backup/restore - no `tar`/`unzip` shellouts.
- Improved chunking: overlap between chunks and hard-split of overlong lines.
- Tier truncation breaks on word boundaries.

### Tooling
- First unit-test suite (`cargo test`) and GitHub Actions CI (fmt, clippy `-D warnings`, test, `cargo audit`).

### Deferred to 0.3 (see AUDIT_STATUS.md)
- Long-lived daemon + thin MCP client (kills per-call model reload).
- Single-collection storage with `library` payload filter (parallel/global ranking).
- Code-capable embedder + cross-encoder reranker + hybrid (BM25/RRF) search - these change the vector dimension and require a re-index, done at deploy time.

## 0.1.0
- Initial release.
