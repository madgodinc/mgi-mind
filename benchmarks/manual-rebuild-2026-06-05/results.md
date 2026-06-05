# Manual memory rebuild — results 2026-06-05

## What was done
- Created library `personal` (memories: 0, will grow via mind_add)
- Registered 66 core predicates with explicit cardinality via MCP `mind_predicate(action=register)`
- Added ~207 hand-curated facts via `mind_fact_add` covering:
  - Phase 1: Mad foundation (identity, location, language, hardware, monitors, OS, tools, preferences, methodology, feedback rules) — 60 facts
  - Phase 2: Active projects (mgi-mind, mgi-pulse, logflow, reread, lindict, agentbox, envy, reunion-site, redmi-pad, Pixel Kingdoms, research) — 60 facts
  - Phase 3: Opinion changes (mgi-mind version chain v0.8→v1.6.4, mgi-pulse v0.1→v0.3, COSMIC→KDE, primary_language JS→Rust, HN account hellban, brain server alive→dead, Aurora active→frozen) — 30 facts
  - Phase 4: Dead/archive (Alrighty, grep.app MCP, audio sinks, perf tweaks, references) — 25 facts
  - Plus: bookkeeping (extraction failure, v1.7 bench cancellation) — 8 facts

## Stats before/after
| Metric | Before extraction (2026-06-04) | After extraction (2026-06-05) | After rebuild |
|---|---|---|---|
| kg_facts | ~0 user-facing | 35273 | 35480 (+207) |
| total_memories | 12626 | 12773 | 12773 |
| libraries | 3 | 3 | 4 (added `personal`) |
| registered cardinalities | 0 | 7960 (high-conf bulk) | 7960 + 66 manual overrides |
| in_doubt_count | 0 | 0 | 0 |

## Verification via mind_fact_query
- `subject=Mad` returns ~200 facts including my new structured ones (mixed with old extractor noise)
- `subject=mgi-mind` returns ~70 facts including version chain + my new structured ones
- `subject=Aurora` returns ~200 facts (old extractor noise dominant; my "frozen permanently 2026-05-29" is one of them)
- `subject=brain server` returns 10 facts including both my "alive" (until 2026-05-29) and "dead permanently" — **temporal conflict preserved**

## Critical findings

### What works (manual rebuild ✅)
- All hand-curated facts visible via `mind_fact_query`
- Cardinality registry correctly populated for temporal cases
- Temporal opinion changes preserved as parallel facts (e.g. brain server alive + dead coexist for history)
- New facts have correct timestamps

### What does NOT work yet
- **Subject fragmentation persists** — `Mad`, `mad`, `Mad God`, `Mad God Inc`, `mad_god_inc` are all separate subjects (extractor created variants, manual rebuild added one more canonical `Mad`)
- Search results dominated by extractor noise on heavily-extracted subjects (Aurora has ~200 facts, 99% from extractor)
- No subject normalization layer — would need NER or alias resolution

### For validity model bench
- **Real conflicts exist now**: brain server has alive + dead in same canonical subject ⇒ duel rule can be exercised
- **Manual N for conflict-bearing pairs**: ~30 (Phase 3 facts)
- Could be the basis for STALE+real bench: use these 30 conflict pairs, measure duel rule precision/recall

## Next steps recommended
1. Write conflict-pair test fixture from Phase 3 facts → run duel rule → measure
2. Decide: extractor cleanup vs leave coexisting (current behavior preserves history)
3. Consider tombstone for old `mad` (lowercase) subjects that confuse query results
