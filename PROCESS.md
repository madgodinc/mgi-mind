# How to read this repo if you care about process

- **Code + tests** — in `src/*.rs`, tests in `#[cfg(test)] mod tests`
  inside each file. Run: `cargo test --release`.
- **Design docs** — `docs/design/v<version>/`. Each milestone has its
  own folder with synthesis, plan, and history of drafts.
- **Feature branches** — `v<version>/phase-N-<name>`. One PR per phase.
- **Benchmarks** — `BENCHMARKS.md` + raw JSON under
  `benchmark/results/`. Numbers are from public synthetic corpora
  (LongMemEval-S etc.), never from author's own memory store.
- **Roadmap** — `ROADMAP.md`.
- **Changelog** — `CHANGELOG.md`.

Author's working memory (`~/mgimind/`) is not in this repo and never
will be.
