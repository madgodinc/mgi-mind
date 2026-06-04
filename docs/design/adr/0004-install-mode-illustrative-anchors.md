# ADR 0004 — Install-mode profiles with illustrative-only anchors

**Status:** accepted (anchors marked TODO(phase-4-calibration)).
**Date:** 2026-06-04.

## Decision

`InstallMode` enum (`ChatOnly` / `DevWithCi` / `MultiTenant`)
selects three per-mode weight anchors for the confidence_score
formula:

| mode | dependants | confirmations | external |
|------|------------|---------------|----------|
| ChatOnly (default) | 0.7 | 0.1 | 0.2 |
| DevWithCi | 0.5 | 0.15 | 0.35 |
| MultiTenant | 0.4 | 0.4 | 0.2 |

Anchors sum to 1.0 by construction. Test `weights_sum_to_one`
enforces it.

**The numbers are placeholders.** They are picked from §6 of the
validity-model synthesis as starting points. A calibrated sweep
against LongMemEval / STALE will move them. Until that sweep is
run (issue #16), `TODO(phase-4-calibration)` markers in the source
flag them as un-calibrated.

## Context

Validity-model synthesis §5 and §6. The same fact's confidence
score is not the same in different deployment contexts:

- In a single-user chat-assistant memory (the default), one user
  saying the same thing twice is almost no evidence. Three of the
  four "diversity axes" decay to zero. `dependants` carries the
  load.
- In a CI loop's memory, deterministic external signals
  (`test_passed`, `code_compiled`) are much stronger than
  conversational repetition.
- In a multi-tenant store, multiple distinct agents reaching the
  same conclusion IS strong evidence (different from one agent
  repeating itself).

A single global weighting can't serve all three. Either it
over-weights `confirmations` (wrong in chat-only) or it
under-weights `external` (wrong in dev-with-CI).

## Reasons for three profiles, not more

- Three is the smallest set that covers the qualitatively different
  use cases from §5. A continuous knob would invite users to invent
  worse defaults.
- More profiles add cognitive overhead at install time. Users have
  to pick; auto-detect has more options to mis-classify.
- The contract test `chat_only_mode_matches_legacy_weight_new`
  pins ChatOnly to the v1.4 hardcoded weights bit-for-bit. New
  modes can be added; ChatOnly cannot drift.

## Why anchors are illustrative

Three classes of evidence point to "needs real calibration":

1. **No real-base sweep has been run.** The numbers come from
   reading §6 and picking values that match the qualitative
   description.
2. **Each constant is documented as a calibration target.** The
   `pub const` declarations carry `TODO(phase-4-calibration)`
   comments so the sweep tooling can find them via grep.
3. **The STALE / LongMemEval sweep is scaffolded** in
   `src/bench_stale.rs::CalibrationOverrides`. It accepts overrides
   for every per-mode anchor and every duel-rule constant. Until
   the dataset adapter lands (issue #16), the sweep returns empty
   reports.

The honest path is "ship the structure with illustrative numbers,
flag the calibration debt, hold the structure stable while the
numbers move". Synthesis §6 explicitly says: "the numeric weights
above are illustrative anchors, not the final formula".

## Auto-detect heuristic

`install_detect::collect` + `install_mode::recommend`:

- `distinct_session_agents ≥ 3` (over last 30 days) → MultiTenant.
- else `external_signal_count_last_7d ≥ 10` → DevWithCi.
- else ChatOnly.

Ordering matters: MultiTenant beats DevWithCi when both apply.
Reason: in multi-tenant, `confirmations` becomes load-bearing
because independent agents reporting the same fact IS evidence.
`external_signals` is secondary in that regime.

The thresholds (3 agents, 10 signals / week) are conservative
cliffs, not gradients. §10 question 6 says mis-classification
cost is silent quality drift — better to default to ChatOnly and
have the user explicitly opt out.

Auto-detect is **informational only.** `mgimind doctor` and
`mgimind config install-mode` print the recommendation; nothing
auto-applies. The user explicitly sets via `mgimind config
set-install-mode <mode>`.

## Trade-offs

- **Users have to pick.** Documentation in `mgimind doctor` output
  surfaces the auto-detect line; v1.6.1 also prints the weight
  breakdown so users can compare modes visually.
- **The anchors will move.** Acceptable — the contract test
  freezes ChatOnly. New modes can be calibrated independently.
- **Three modes don't cover every use case.** A "single-user
  with occasional manual outcome calls" profile sits between
  ChatOnly and DevWithCi. Acceptable for v1.5 / v1.6 — the user
  can edit weights manually via config.json once issue #16 lands.

## Implementation

`src/install_mode.rs`:

- `InstallMode` enum with `weights()` method.
- `parse(s)` accepts kebab-case and snake_case (forgiving).
- `ALL` array for CLI enumeration.

`src/install_detect.rs`:

- `collect(config)` pulls real counts from the system.
- `recommend(inputs)` is pure; testable independently.
- `distinct_session_agents_in(&Path)` accepts an explicit
  directory so unit tests can use a tempdir without racing the
  shared `MGIMIND_HOME` env var.

`src/duel.rs::weight_new_for_mode(inputs, mode)`:

- Uses `mode.weights()` instead of hardcoded constants.
- v1.4 `weight_new` is preserved as a shim that calls
  `weight_new_for_mode(_, ChatOnly)` — pinned bit-for-bit by the
  contract test over 256 input combinations.

`src/confidence.rs::confidence_score(inputs, mode)`:

- Same shape, plus inheritance penalty applied uniformly.

`src/config.rs::MindConfig`:

- `install_mode: InstallMode` with serde default to ChatOnly.
- Pre-v1.5 configs without this field deserialise to ChatOnly,
  preserving v1.4 behaviour.

## Tests pinning the design

- `install_mode::tests::weights_sum_to_one` — every mode sums to
  1.0 ± 0.001.
- `install_mode::tests::round_trips_through_json` — kebab-case
  serde.
- `install_mode::tests::per_mode_emphasis_preserved` — ordering
  invariants (ChatOnly emphasises dependants, DevWithCi raises
  external, MultiTenant raises confirmations).
- `duel::tests::chat_only_mode_matches_legacy_weight_new` —
  256-input bit-for-bit contract.
- `duel::tests::dev_with_ci_mode_lifts_external_signal_weight` —
  proves the knob does something.
- `duel::tests::multi_tenant_mode_lifts_confirmation_weight` —
  same for the third mode.
- `config::tests::pre_v15_config_defaults_to_chat_only` — pre-v1.5
  configs round-trip cleanly.

## Alternatives considered

- **One global weight matrix.** Rejected per ADR motivation.
- **Continuous knob (no enum, raw numbers in config).** Rejected
  — invites worse defaults than the three canned profiles.
- **More profiles** (`AcademicResearch`, `PersonalKnowledge`,
  etc.). Rejected for v1.5 — three is the smallest set that
  covers §5 qualitative diversity. Add more after the calibration
  sweep shows real misclassification.

## Future revisions

- Calibration sweep (issue #16) lands real numbers; ChatOnly stays
  pinned, DevWithCi and MultiTenant adjust.
- v2.0 may add a per-fact mode override for facts written through
  specific MCP tools (e.g. always-CI signals from `mind_outcome`
  use the DevWithCi `external` weight even on a ChatOnly install).
- v2.x may explore continuous interpolation between profiles for
  hybrid deployments.
