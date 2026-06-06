# Contributing to mgi-mind

Tests pass, `cargo fmt` ran, CHANGELOG updated, no AI co-authors in the
commit. Details below.

## Project layout

Single crate, one binary (`mgimind`). Sources:

- `src/engine` — none. mgi-mind has no separate engine / binary
  split; the surface is small enough that one crate works. If we
  grow that, the split will mirror mgi-pulse.
- `src/cli.rs` — clap-derived CLI surface. Every public-facing flag
  lives here.
- `src/mcp.rs` — Model Context Protocol surface. JSON-RPC over
  stdio; routes to the same handlers the CLI uses.
- `src/storage.rs` — Qdrant client wrapper. All reads/writes go
  through here.
- `src/knowledge.rs` — facts collection, cardinality registry,
  duel rule entry point.
- `src/doubt.rs` — v1.4 Phase 3 doubt window + v1.5 Phase 8
  active re-test loop.
- `src/confidence.rs` — pure §6 / §8 step 8.2 formulas.
- `src/install_mode.rs` — v1.5 Phase 6 per-mode anchors.
- `src/outcome.rs` — v1.5 Phase 7 typed external signals.
- `src/audit.rs` — append-only audit log (every state transition).

```sh
cargo build --release            # release binary (~30 MB stripped)
cargo build --release --features extractor   # + Qwen 2.5 GGUF auto-extractor
cargo test --release             # 290 unit + 6 integration tests
cargo clippy                     # should stay clean on stable
cargo fmt                        # required before sending a patch
```

MSRV is the workspace's `rust-version` (currently 1.84). Stable only.

## Architecture rules

Three rules the model relies on. Please don't invert them:

1. **Mechanism 1 invariant: never hard-delete.** A fact that loses
   a duel becomes `Stale` with `valid_until`; never `Delete`. The
   re-test pass returns `RetestTransition::{NoChange, PromoteToDoubt,
   RecoverFromDoubt}`. There is intentionally no `Remove` variant.
   Soft-decay only goes through `consolidate --soft-decay` and
   moves facts into the existing v0.11 quarantine.

2. **§10 q5 guarantees on the background loop.** Any change to
   `spawn_background_retest_loop` must preserve:
   - (a) never concurrent with MCP tool call — `is_mcp_busy()`
     checks at outer wake AND between facts in the walk.
   - (b) hard per-tick cap — `select_retest_candidates(_, cap)`
     enforces `cap` with an assert.
   - (c) load-aware cadence — `loadavg_multiplier()` reads
     `/proc/loadavg` on Linux.

3. **All numeric constants are illustrative until calibrated.**
   Anything with a `TODO(phase-4-calibration)` comment is a tunable
   for the STALE bench sweep. Don't ship a PR that hardcodes a new
   one without flagging it. Please make it `pub const NAME: f32 =
   …;` so the calibration tooling can find it via grep.

## Adding a new MCP tool

Three places to touch:

1. **Tool surface in `src/mcp.rs`** — add the JSON schema in the
   `tool_definitions()` array, plus a match arm in `call_tool`.
   Update both `tools/list` count tests
   (`exposes_consolidated_and_legacy_tools`,
   `tools_list_returns_v1_5_surface`) — they pin the surface size.

2. **CLI mirror in `src/cli.rs`** — add a `Commands::` variant and
   a handler. The CLI surface should reach every MCP tool so you can
   debug from a terminal.

3. **Tests** — at minimum, unit tests on the pure helpers (schema
   parse, formula). Integration tests in `tests/cli_integration.rs`
   exercise the binary against a real Qdrant — gated on
   `MGIMIND_IT_QDRANT=<port>`, so plain `cargo test` skips them.

## Adding a new install mode

If you have a use case the three v1.5 modes don't cover:

1. Open a Discussion before writing code. Anchors get tuned by
   bench, not by guess.
2. Add the variant to `install_mode::InstallMode` plus weights in
   `weights()`.
3. Add an auto-detect heuristic in `install_detect::recommend`.
4. Update the contract test
   `chat_only_mode_matches_legacy_weight_new` — the rule is that
   `weight_new_for_mode(_, ChatOnly)` must equal v1.4 `weight_new`
   bit-for-bit. Your new mode just adds another arm; ChatOnly stays
   frozen.

## Branch model

- `main` — green. Every commit on main passes `cargo test --release`
  and `cargo build --release --features extractor`.
- `vX.Y/topic` — feature branches per release scope. Phase 7 in v1.5
  was `v1.5/phase-7-mind-outcome`; v1.6 step 1 was
  `v1.6/phase-1-batched-payload-reads`.
- PRs land via "Merge with merge commit" (not squash) — the per-step
  commit history is the audit trail. Rebase before merge if the PR
  has been around for more than a couple days.

## Commit messages

Header line under 72 chars. Body wraps at 72. Subject prefixes:

- `feat(area):` — new behaviour visible from the surface.
- `fix(area):` — bug fix.
- `perf(area):` — same behaviour, faster.
- `chore(area):` — build / CI / formatting / dead_code.
- `docs(area):` — README, CHANGELOG, inline comments.
- `test(area):` — pure test additions.

`area` is one of `cli`, `mcp`, `storage`, `knowledge`, `doubt`,
`confidence`, `install-mode`, `outcome`, `audit`, `extractor`. Look
at the existing log for the conventional set.

**No AI co-authors.** mgi-mind is a personal-and-OSS hybrid; commits
go to my git history without a Co-Authored-By trailer to Claude or
otherwise. The PR description is fine to say "this was drafted with
Claude Code" if you want — the commit history stays clean.

## CHANGELOG

Every user-visible change gets a CHANGELOG.md entry under the
current version heading. Bugfix that only affects internal tests —
optional. Anything that affects MCP surface, CLI flags, formula
behaviour, or build/install — required.

## Code of Conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Short version: be
useful, be honest about limits, no harassment, no asshole behaviour.

## Where to ask

- **Bugs / concrete features** — open an Issue (`bug_report.md` or
  `feature_request.md` template).
- **Questions / design** — open a Discussion. Issues are for
  actionable items; "how do I…" / "what about…" stays in Discussions.
- **Security** — email instead of public issue. Address in CODE_OF_CONDUCT.

## Past contributors

mgi-mind is mostly a solo project, but a few outside contributions
shaped the design. Listed here because the issue / PR history alone
does not show why they mattered:

- **[@spikefcz](https://github.com/spikefcz)** — PR #2 (closed,
  superseded by v1.4 Cardinality enum + duel rule). The PR showed
  that audit #13 (single-valued fact accumulation) mattered to
  someone besides me, which is part of why v1.4 prioritised the
  broader Mechanism 1 fix. I closed it once the generalised
  solution shipped. The conversation is
  at https://github.com/madgodinc/mgi-mind/pull/2.

If you contributed something and you are not listed here, open an
Issue or nudge me in Discussions. I am bad at self-noticing.
