# Changelog

## Unreleased — v1.7 candidate

### BREAKING (serve-http): structured JSON is the default for read routes

**PR [#45](https://github.com/madgodinc/mgi-mind/pull/45).**

`/memory/search` and `/memory/recall` now return structured JSON by
default instead of the `{ok, result: "<text>"}` envelope:

- `/memory/search` → `{ok, results: [{id, score, content, library,
  author, created_at, source}, ...]}`
- `/memory/recall` → `{ok, facts: [...], memories: [...],
  procedures_text: "..."}` (silos kept separate; `procedures_text` is a
  rendered string, the name says so)

The old `result` text field is gone from these two routes under the
default. A non-Python client that read `.result` must either pass
`format: "text"` to keep the old rendered envelope, or read the new
fields. `format` is validated: an unknown value (e.g. `"yaml"`) is a 400,
not a silent fall-through. The `serve-http` surface is new (landed days
ago), so the blast radius is near zero; the contract is not frozen while
the project is pre-1.0, and JSON-by-default is the right shape for the
agent callers this surface exists for.

The Python client (0.2.0 → 0.3.0) tracks this: `MemoryResult` gains
`.results` / `.facts` / `.memories` / `.procedures`, is iterable over
hits, and has `len()`; `str(result)` still yields a prompt-ready block.

### Fixed: duel rule was silently bypassed at the read path

**Issue [#25](https://github.com/madgodinc/mgi-mind/issues/25) /
PR [#26](https://github.com/madgodinc/mgi-mind/pull/26).**

`query_facts` and the other fact read paths filtered only on
`valid="true"`, not on `status`. `dampen_loser` writes `status="stale"`
without touching `valid`, so dampened losers from the duel rule
remained visible in every query. The headline conflict-resolution
feature appeared broken from a user perspective even when the
internal logic was firing correctly.

Fix: every fact read path now excludes `status="stale"` AND
`status="superseded"`. Affected:

- `knowledge::query_facts`
- `knowledge::find_facts_by_subject_predicate`
- `knowledge::list_all_facts` (used by stats / audit)
- `knowledge::list_top_dependants_facts` (used by `mgimind facts list`)
- `cli.rs` doctor's `facts_summary` loop

Two new integration tests pin the regression:

- `duel_rule_dampens_loser_on_single_cardinality` — register Single
  cardinality + add winner + add conflicting + query, asserts only
  the winner is returned.
- `multi_cardinality_allows_coexistence` — same flow with Multi,
  asserts both facts coexist.

Both vectorless, ~0.2 s each, run on every CI push.

### Added: `EntryStatus::Superseded` (separate from Stale)

[ADR 0005](./docs/design/adr/0005-superseded-vs-stale-status.md).

`Stale` = lost a contradiction duel (`dampen_loser`).
`Superseded` = overtaken by a newer entry in a `TemporalSingle` chain
(`mark_superseded`). Both are default-hidden from queries; both keep
`valid="true"` and a `valid_until` timestamp. The difference matters
for future `mind_history` and audit-replay tools that should render
the duel outcome and the temporal progression differently.

Wire format: lowercase string `"superseded"`.

### Added: `mgimind migrate-v14 redo-duels`

Retroactive walk for legacy KGs from before the read-path fix.
Scans every `(subject, predicate)` cluster, identifies Single /
TemporalSingle predicates with > 1 active fact, runs the duel rule:

- Single clusters: losers dampened (`status=stale`).
- TemporalSingle clusters: older entries marked superseded
  (`status=superseded`).

Idempotent. Dry-run is the default; `--apply` writes. `--limit N`
samples the top-N biggest clusters first.

On a real KG of 35 480 facts the walk found 31 conflict-bearing
clusters: 19 Single (44 losers dampened) + 12 TemporalSingle (38
losers superseded). Headline real-data conflicts (e.g. `Aurora
has_status active → frozen`, `mgi-mind has_version v0.8.0 → … →
v1.6.4`) now collapse to the canonical answer.

### Added: 2026-06-05 blog post

`docs/blog/2026-06-05-i-found-my-headline-feature-was-broken.md` —
post-mortem narrative covering the bug discovery, localization,
fix, retroactive walk, and what 290 passing unit tests can't tell
you about a user-facing read path that drifted out of sync with
the write model.

### Validity model: six axes, every stored field now has a reader

Three review rounds hardened the memory-validation model across six
axes: provenance, write discipline, temporal correctness, concurrency,
forgetting, and retrieval fidelity. The guiding rule each round: a
stored field that nothing reads back is dead weight, so every change
ships with the consumer that distinguishes the new behavior. PRs
[#57](https://github.com/madgodinc/mgi-mind/pull/57)–[#62](https://github.com/madgodinc/mgi-mind/pull/62).

**Temporal, point-in-time fact queries (PR #57).** `fact query
--as-of <timestamp>` answers what a `(subject, predicate)` axis held at
a past instant. The reader gives `valid_until` a job: before this,
`dampen_loser` and `mark_superseded` wrote the timestamp and no query
ever consulted it. A fact counts as valid at instant `t` over the
half-open interval `[created_at, valid_until)`.

**Forgetting, coldness surfaced in browse (PR #59).** `browse` and
`list_filtered` stamp a recency-weighted `coldness` score on each
record, so decay shows up before consolidation acts on it.
`consolidate --archive-cold` archives the cold tail; the destructive
prune stays a separate, narrower `cold_prune` so an archive sweep
deletes only what the operator asked for.

**Retrieval, per-query rerank override (PR #60).** `search --rerank /
--no-rerank / --rerank-top-k N` overrides the configured reranker for
one call, so an agent trades latency against precision per request, not
per deployment.

**Temporal cardinality inference (PR #61).** `migrate-v14` now proposes
`TemporalSingle` alongside `Single` by reading a subject's superseded
history, so a predicate that changes over time gets a cardinality that
fits a value sequence and a single-value axis stays a conflict.

**Concurrency, the torn-Active bug closed across every writer (PRs
#58, #62).** Two processes adding to the same `Single` axis could each
duel against a stale read and both end Active, violating the
one-canonical-value invariant. PR #58 closed the add-vs-add case with a
cross-process advisory file lock (`fs2` flock on
`data_dir/.facts.lock`, released by the OS on process death, acquired
on a blocking thread). PR #62 extended that lock to every path that
flips a fact's status or validity: `invalidate_fact_authored` now takes
it, and `redo-duels --apply` re-reads each axis under the lock,
recomputes the winner, then retires the losers, so the decision and the
write are one atomic step rather than a check against an unlocked
snapshot. The duel winner order is total (newest `created_at`, `id`
breaks ties) and lives in one shared comparator, so the dry-run plan
and the applied result agree even when two facts share a timestamp.
`set_fact_payload_field` stays lock-free by design: it writes only
keys disjoint from status/validity, and Qdrant's `set_payload` merges
named keys rather than replacing the payload, so a concurrent status
write survives. A live run collapsed a torn `Single` axis from two
Active facts to one; the loser kept its row with `status=stale` and a
`valid_until`, and a second run found nothing left to do.

### Added: `mgimind calibrate` — behavioral metric for the validity model

The duel rule and doubt window had no measured number, so the README
claim was a promise. `mgimind calibrate` runs a corpus of realistic
conflict situations through the live duel formulas (`entrenchment`,
`weight_new`, `resolve_duel`) and reports how many land on the outcome a
person would expect: a fresh unsupported claim can't overturn an
entrenched belief, a CI signal can, repetition alone coexists. 14 of 15
scenarios match intent; the one divergence (the inheritance discount is
too weak to hold a strong memory-sourced challenger to Contested against
a core belief) is printed with its reason, not hidden. The divergence set
is frozen in a test, so a tuning-constant change that shifts a scenario
across a band trips CI. This measures the *shape* of the model, separate
from retrieval recall (R@k).

### Stats and tests

- 344 unit tests pass (up from 290; +49 across circle-3, +5 for the
  calibration suite).
- 8 CLI integration tests pass.
- CI green on macOS, Ubuntu, and Windows, plus the real-Qdrant and
  bundled-Qdrant integration suites and `cargo-audit`.
- Build remains warning-free; `clippy` clean.

## 1.6.4 — Windows fix + doctor --fix cardinality + ADRs + SECURITY policy

This release closes the v1.5 honest limit on cross-platform CI plus
ships the developer-facing scaffolding that pulse-style projects
have.

### Issue [#23](https://github.com/madgodinc/mgi-mind/issues/23) — Windows stack overflow

`tokio::main` runs `block_on` on the process's main thread, which
uses the OS default stack budget. Windows defaults to 1 MB; the v1.5
background re-test loop's futures (MindConfig clone + payload
HashMaps + Vec<String> candidates + per-fact futures) overflow it.

Fix (two layers in `src/main.rs`):

1. Re-launch `main` on `std::thread::Builder` with 8 MB stack —
   fixes the process main thread on every platform.
2. Build the tokio runtime with `thread_stack_size(8 * 1024 * 1024)`
   — every worker thread (where `tokio::spawn` lands) gets the same
   8 MB.

8 MB matches the Linux default — the most-tested configuration.
After this lands all six CI jobs go green (Linux / Windows / macOS
× fmt+clippy+test + Linux integration + Windows integration +
cargo-audit).

### `mgimind doctor` detects pending cardinality proposals

After extraction lands hundreds of predicate cardinality proposals
in `$MGIMIND_HOME/migration/cardinality-proposals.json`, the user
had to remember to run `migrate-v14 cardinality --apply` to register
them. `mgimind doctor` now surfaces the count:

```
[INFO] 1096 High-confidence cardinality proposal(s) waiting — run
       `mgimind doctor --fix` or
       `mgimind migrate-v14 cardinality --apply`
```

`mgimind doctor --fix` bulk-registers every pending High-confidence
proposal via the same path the migrate-v14 path uses.

### `mgimind doctor` and `mgimind config install-mode` show weight breakdown

Both surfaces now print the install-mode AND the per-mode anchors:

```
[OK]   install-mode: chat-only [d=0.70 c=0.10 e=0.20] (matches auto-detect)
```

```
install-mode: chat-only [dependants=0.70 confirmations=0.10 external=0.20]
```

### Project documentation

- **CONTRIBUTING.md** — project layout, build commands, three
  architecture rules (Mechanism 1 invariant, §10 q5 guarantees,
  illustrative-until-calibrated constants), how to add an MCP tool,
  branch model.
- **CODE_OF_CONDUCT.md** — pragmatic compression of Contributor
  Covenant 2.1.
- **SECURITY.md** — vulnerability disclosure policy with scope
  (vault, extractor, MCP surface, Qdrant binary, audit log) and
  out-of-scope (shell access, DoS via large inputs, MCP client
  misbehaviour).
- **`.github/ISSUE_TEMPLATE/`** — bug_report.md + feature_request.md
  + config.yml routing questions to Discussions.
- **`docs/design/adr/`** — four foundational ADRs (Cardinality enum,
  Mechanism 1 invariant, §10 q5 guarantees, install-mode anchors).
- **`benchmarks/v0.14.3-gpu/`** — pre-v1.4 retrieval baseline
  (R@5 = 99.2%) committed for STALE bench comparison.
- **`docs/blog/2026-06-04-validity-model.md`** — draft technical
  post explaining v1.4 / v1.5 / v1.6 design.
- **`.editorconfig`** — consistent indent / EOL across editors.

### CLI surface

- `mgimind bench-stale` + `mgimind bench-stale-sweep` — CLI scaffold
  for issue [#16](https://github.com/madgodinc/mgi-mind/issues/16)
  calibration tooling. The STALE protocol adapter is still TBD; the
  CLI plumbing and sweep grid are in place.

### Tests

- 290 unit + 6 integration tests pass on **all three OSes**.
- Cleaned up test serialisation around `DOUBT_WINDOW_FLAGGED`
  global registry — four registry-touching tests now lock
  `SERIAL_LOOP_TEST` to prevent races on the macOS runner.

### Documentation issues opened

- **#23** — Windows stack overflow (closed by this release).
- **#24** — `ROADMAP.md` is stale (last version mentioned is v1.2;
  we shipped v1.3 through v1.6.4). Tracked for v1.7.

### Migration notes

None. v1.6.4 ships only the Windows fix + scaffolding; no semantic
changes to formulas, MCP surface, or payload shape.

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


---

## Earlier releases (v0.1.0 – v0.14.3)

Moved to [CHANGELOG-ARCHIVE.md](CHANGELOG-ARCHIVE.md) to keep this file
readable. That covers the audit-hardening rounds (#16–#23), the hybrid
search and reranker work, and the benchmark harness that produced the
R@5 = 98.2% headline.
