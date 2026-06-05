# KG Cleanup Final State — 2026-06-05

## Before / After

| Metric | Before cleanup | After cleanup |
|---|---|---|
| KG facts (active + history) | 35506 | 291 |
| Active facts | 35421 | 198 |
| Stale + Superseded (history) | 82 | 93 |
| Unique subjects (active) | 9325 | ~70 |
| Extractor noise % | 98.8% | 0% |

## Backups (3 snapshots, all 130MB)

- `mgi-mind-backup-2026-06-04-164537.tar.gz` — yesterday baseline
- `mgi-mind-backup-2026-06-05-post-extraction.tar.gz` — after 11h extraction
- `mgi-mind-backup-2026-06-05-pre-cleanup.tar.gz` — before today's destructive cleanup

If anything needed from the deleted 35k facts: `tar xzf <backup>.tar.gz -C ~` overwrites qdrant + ~/Brain/memory.

## Canonical answers verified

| Question | Answer (single canonical) |
|---|---|
| Who is Mad? | solo founder of Mad God Inc |
| Where does Mad live? | Kazakhstan |
| What desktop does Mad use? | KDE Plasma on X11 |
| Aurora status? | frozen permanently (since 2026-05-29) |
| Brain server status? | dead permanently (2026-05-29) |
| mgi-mind current version? | v1.6.4 |
| mgi-pulse current version? | v0.3.0 |
| Mad's primary language? | Rust |
| HN account MadGodInc? | hellbanned |

All answers are single-fact — no duplicates, no contradictions surfacing.

## What's in the 198 active facts

- ~60 about Mad personally (identity, location, languages, hardware, preferences, rules)
- ~70 about active projects (mgi-mind, mgi-pulse, logflow, reread, lindict, agentbox, envy, reunion-site, redmi-pad-port)
- ~30 about dead/archived things (Aurora, brain server, memorypalace, Tyan brick)
- ~15 about references (Alrighty, grep.app MCP, audio sinks, perf tweaks)
- ~25 about meta (cleanup itself, PR #26, benchmarks, backups)

## What's in the 93 history facts

- 44 dampened losers from Single duels (e.g. Aurora active → frozen)
- 38 superseded entries from TemporalSingle chains (e.g. mgi-mind v0.8.0..v1.5.0 → v1.6.4)
- ~11 other historical entries

All queryable via `mind_history` (when implemented) or direct Qdrant scroll with status filter.

## Library state

| Library | Count | Purpose |
|---|---|---|
| `claude-musings` | 129 | Claude reflections, structured by date |
| `personal` | 0 | Reserved for Mad's hand-curated memories (facts live in KG separately) |
| `projects` | 12626 | Extracted raw source memories (READMEs, CHANGELOGs). Kept as archive of what was extracted from. Renamed to `extracted-raw` conceptually (waiting on issue #28 for proper rename CLI) |
| `strategy` | 18 | Strategy notes |

## Open follow-ups (issue #28)

- `mgimind rename <old> <new>` for libraries
- `mind_history` MCP tool to expose Superseded chain entries
- Auto-status assignment in `add_fact` MCP wrapper (current bug: missing status field on new facts breaks the must_not filter sometimes)
