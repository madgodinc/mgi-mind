# MGI-Mind: instructions for the AI

You are connected to MGI-Mind, the user's self-hosted long-term memory. It lets
you remember across sessions: you write things down as you work and read them back
by meaning. Everything is local to the user's machine. Read this file once at the
start; it is your operating manual.

You reach MGI-Mind through MCP tools named `mind_*` (listed at the end). There is
also a `mgimind` CLI, but as the assistant you use the MCP tools. When this file
says "search", it means call `mind_search` (or `mind_recall_all`).

## The one rule

**Assume your context is stale. Verify against memory before you act.** Before you
make a factual claim about the user's projects, people, environment, preferences,
or past decisions, or before you act on one, call `mind_search` first and answer
from what comes back. Treat your own recollection as a draft to check, not a
source. This is the whole point of the system.

This is opt-out, not opt-in. The default is to search; you skip it only for
general knowledge and for things the user just told you in this conversation. If
you are unsure whether a lookup is worth it, `mind_should_search` classifies a
query and tells you.

## Search triggers (when the one rule fires)

The rule above is the principle; this is the operational checklist. MCP cannot
force a search, so these triggers give the model explicit signals to override its
instinct to answer from priors. Project names live in the namespaces returned by
`mind_context` (libraries), not in a separate config.

**Priority 1 - search before answering:**

| Trigger | Example | Why |
|---|---|---|
| Named project / library | "how's the Aurora bench going?" | name = handle for stored context |
| Named person / handle | "what did Alrighty say about X?" | identity = stored fact |
| Stated preference / rule | "do I prefer Go or Rust?" | preferences live in facts |
| Decision recall | "what did we decide about the schema?" | decisions are durable notes |
| Meta-cue about memory | "did I tell you about X?", "do you remember Y?", "you already know this" | the user is testing the store |
| Negation to verify | "isn't it X?", "it's NOT Y, right?" | falsification needs a lookup, not a guess |
| Cross-session reference | "like last time", "as before", "the file we were editing" | by definition not in current context |

**Priority 2 - search before assuming:**

| Trigger | Example |
|---|---|
| Unfamiliar acronym / proper noun | "fix the bug in PXK" |
| Vague "the" referring to prior context | "fix the deploy script" |
| User asks "what do you know about X" | "what do you know about my setup?" |

If none of the above fires, answer directly. Over-searching wastes tokens and
slows the response. There is no "never search" tier on purpose: a missed search
costs more than a redundant one.

## Session protocol (do this every session)

**At the start:**
1. `mind_session(action="last")` with your agent name (e.g. `agent: "claude-code"`)
   to read your last session summary.
2. `mind_session(action="start")` with the same agent name to begin logging.
3. `mind_context` for a compact briefing: recent facts, the libraries that exist,
   and a quarantine digest if anything is set aside. Greet the user with the
   relevant context.

Always use the **same agent name** for start and end. Sessions are per-agent so two
assistants never overwrite each other's log.

**During the session:**
- Past / preferences / projects question -> `mind_search` (or `mind_recall_all`) first.
- The user shares something worth keeping -> `mind_add` (or batch several via `mind_ingest`).
- The user states a durable fact -> `mind_fact(action="add")`.
- The user asks what you know about X -> `mind_fact(action="query")`.
- You hit an error you have seen before, or start a task that tends to fail ->
  `mind_recall` for a known fix before trying from scratch.

**At the end:**
- `mind_session(action="end")` with the same agent name and a `summary` of what was
  done and what is next (keep it under ~200 words). Continuity depends on this.

## What goes where

| Kind of information | Tool | Example |
|---|---|---|
| Permanent rule / identity / workflow | the AI config file (CLAUDE.md, etc.) | "Always use Rust for new projects" |
| Durable structured fact | `mind_fact(action="add")` | user -> prefers -> Rust |
| Details, notes, context | `mind_add` (or `mind_ingest`) | "The staging DB is Postgres 16 on db-staging:5432" |
| A solved error / how-to-fix | `mind_learn` | error -> fix, recalled by `mind_recall` |
| A secret (password, key, token) | the vault, in the user's terminal | never through MCP |

When the user sets a **permanent rule or preference** ("always use X", "my name is
Y", "never auto-commit"):
1. Store it as a fact with `mind_fact(action="add")`.
2. Offer to also add it to the AI config file (CLAUDE.md for Claude Code,
   `.cursorrules` for Cursor, `.clinerules` for Cline) so every future session has
   it in the system prompt at zero lookup cost. Show exactly what you would add,
   append only, and ask first.

## Facts and supersession (important)

Facts are triples: subject, predicate, object. Adding the same triple again
overwrites it (dedup). The store does **not** automatically retire an old fact when
a single-valued one changes. So if the user switches preference, moves city, or
renames a thing, retire the old value yourself, or you accumulate contradictory
facts that all read as valid:

1. `mind_fact(action="query")` for the subject to find the old fact and its id.
2. `mind_fact(action="invalidate")` the old id (soft delete; it stays on disk,
   hidden from queries).
3. `mind_fact(action="add")` the new value.

For genuinely multi-valued predicates (the user likes several things), just add;
do not invalidate the others.

A predicate's cardinality controls whether a new value conflicts with an old one.
`mind_predicate` registers a predicate as `single` (one current value, a new one
supersedes), `temporal-single` (one current value, history kept and queryable by
date), or `multi` (values coexist). Unregistered predicates default to `multi`.
Register a predicate as `single` or `temporal-single` when contradictory values
should not pile up.

## Search and tiers (spend tokens wisely)

`mind_search` is hybrid (semantic + exact-term) and reranked. Use `tier` to control
how much text comes back, not which results:
- `tier: 1` - about 100 characters per hit. Quick lookups.
- `tier: 2` - about 500. The default, good balance.
- `tier: 3` - full text. Only when you actually need the detail.

Start at tier 1 or 2 and escalate to tier 3 only when needed. Use `library` to scope
a search to one namespace, and `limit` to cap the number of hits. `mind_recall_all`
searches memories, facts, and procedures together when you want everything the store
knows on a topic in one call.

## Secrets: vault is terminal-only

No secret value crosses the MCP channel, in either direction. The `mind_vault` tool
returns **terminal instructions**, never the secret, and never takes the secret
value as a tool argument (it would otherwise land in the process command line).

- The user wants to store a secret -> tell them to run, in their terminal:
  `mgimind vault store <key> <value> --category <password|ssh|api-key|token>`
- The user wants a secret -> tell them to run: `mgimind vault get <key>`
- The user wants to see what keys exist -> `mind_vault(action="list")` renders the
  list-keys instruction.
- Never store a secret with `mind_add`. Never try to read or echo a secret yourself.

## Auto-ingest: you are the judgment (`mind_ingest`)

You are the memory's extractor. As you work, when a few things accrue that are worth
keeping, send them in one `mind_ingest` call with a `candidates` array. You already
judged they matter, so YOU are the significance gate. Each candidate is one of:
`{"type":"memory","content":"..."}`, `{"type":"fact","subject":"...","predicate":"...","object":"..."}`,
or `{"type":"procedure","trigger_error":"...","fix":"...","context":"..."}`. The
server secret-scrubs each one and skips near-duplicates of what's already stored, so
you don't have to dedup yourself. (A dumb client with no judgment can instead pass
`raw` text for a weak marker-based extractor; as a capable agent, prefer
`candidates`.)

The relevance gate may route a weak candidate to quarantine instead of storing it.
Re-asserting the same content is the signal to promote it. Re-asserting content that
is already stored keeps it live (the store will not demote a memory you write again).

This does not replace consolidation: the user runs `mgimind consolidate` (CLI/cron)
to merge near-duplicates and report stale entries. Auto-ingest writes; consolidation
keeps the store from bloating.

## Procedural memory: learn from fixes (`mind_learn` / `mind_recall`)

When you solve an error, record the lesson: `mind_learn` with the `error`, the `fix`,
and a short `context`. The error signature is normalized (paths, line numbers,
addresses stripped), so the same error matches later regardless of those details.
Leave `verified` false; a manual lesson has no proof.

Before grinding on an error or a recurring task, `mind_recall` it. Ranking blends
relevance with trust: a verified fix and a proven one each get a boost, and a
repeatedly-failing one gets demoted, but the boost tips a close call rather than
overriding relevance, so a strongly-matching unverified fix still surfaces. After you
reuse a recalled fix, report the outcome:
- If a test passed or the code compiled after applying it, call
  `mind_outcome(memory_id=<procedure id>, signal_type=test_passed)`. A deterministic
  success both counts the win AND marks the playbook verified, which earns it the
  trust boost on the next recall.
- For a plain worked/failed report with no such signal, `mind_procedure_outcome
  (worked: true/false)` bumps the counters (a failure demotes the fix) but does not
  verify. Only a real signal does that.

## Saving externally-sourced snippets (`mind_provenance_add`)

When you just produced a snippet via a code-search or doc-search MCP (`mcp.grep.app`,
Sourcegraph, GitHub code search, local `ripgrep`, etc.) and want it durable, use
`mind_provenance_add`, not `mind_add`. Required fields: `snippet` (plain UTF-8, no
HTML), `origin_url` (https, host must be one of github.com / gitlab.com /
bitbucket.org / sr.ht / codeberg.org / grep.app / sourcegraph.com), and
`search_tool_used` (where you found it in THIS session). Optional locators:
`library` (defaults to `external-snippets`, must exist, `mind_library(action="create")`
it first), `repo` (`owner/repo`), `file` (no `/`, no `..`), `line_range` (`42` or
`42-58`), `lang`, `note`.

The dedup key includes `origin_url` and `line_range`, so the same snippet from two
different repos is stored as two records (each with its own citation). The tool is
strictly local: no HTTP, no enrichment, no HTML stripping. Do **not** invent
provenance fields from memory; fill them only from the search result you literally
just saw, or the validation rejects the call.

## Your MCP tools

The store exposes consolidated verbs with an `action` argument. Prefer them. The
older single-purpose tools (`mind_fact_add`, `mind_quarantine_list`,
`mind_session_start`, `mind_vault_get`, `mind_create`, and their siblings) still work
but are deprecated and slated for removal in v2.0.

Memory: `mind_search`, `mind_recall_all` (memories + facts + procedures in one call),
`mind_should_search` (advisory: should I search this query?), `mind_add`,
`mind_provenance_add`, `mind_ingest`, `mind_history`, `mind_context`,
`mind_visualize` (open the 3D memory viewer).

Quarantine (entries the relevance gate filtered, hidden from `mind_search`):
`mind_quarantine` with `action`: `list` (newest first, optional library), `show` (full
content + gate reason by id), `promote` (the gate was too strict, move it to ordinary
memory), `expire` (the gate was right, drop it; only ever touches quarantined points,
never live memory, recoverable from the audit log). `mind_context` surfaces a
by-reason digest when the quarantine is non-empty; follow it with `mind_quarantine`.

Libraries: `mind_library` with `action` create / list / delete. `mind_stats` for
counts.

Facts: `mind_fact` with `action` add / query / invalidate. `mind_predicate` for
cardinality (single / temporal-single / multi).

Procedures: `mind_learn`, `mind_recall`, `mind_outcome` (typed signal:
test_passed / code_compiled, both counts and verifies), `mind_procedure_outcome`
(plain worked/failed, counts only).

Sessions: `mind_session` with `action` start / last / end.

Vault: `mind_vault` with `action` get / store / list (all return terminal
instructions, never the secret).

Web: `mind_web` (read a page as Markdown, if the `crw` tool is installed).

Data / maintenance: `mind_import`, `mind_export`, `mind_doctor`, `mind_consolidate`
(preview only: a dry-run count of duplicates and cold entries; the destructive
`mgimind consolidate --apply` stays on the CLI). Use `mind_consolidate` when the user
asks "how much duplicate memory do I have?" before suggesting they run the CLI.

Admin actions are CLI-only (the user runs them): `mgimind serve`, `mgimind migrate`,
`mgimind drop`, `mgimind backup` / `restore`. The MCP server itself is `mgimind mcp`,
which the user wires into their AI client once; it starts Qdrant automatically.

## A good session, end to end

```
(start)
  mind_session  action=last  agent=claude-code   -> "last time: set up CI, next: write tests"
  mind_session  action=start agent=claude-code
  mind_context                                    -> recent facts + libraries (+ quarantine digest)
  "Welcome back. Last time we set up CI; you wanted to write tests next."

(user: "what DB are we on for staging?")
  mind_search "staging database"                  -> "Postgres 16 on db-staging:5432"
  -> answer from that, do not guess.

(user: "from now on use Go, not Rust")
  mind_fact action=query "user"                   -> finds  user -> prefers -> Rust  (id X)
  mind_fact action=invalidate X
  mind_fact action=add "user" "prefers" "Go"
  -> "Noted. Want me to put 'prefer Go' in CLAUDE.md too?"

(user: "save the prod API key")
  -> "I can't take secrets here. In your terminal:
      mgimind vault store prod-api-key <value> --category api-key"

(end)
  mind_session action=end agent=claude-code summary="Switched preference to Go;
  confirmed staging DB. Next: migrate the Rust snippets in docs."
```

## Rules of thumb

1. Verify before you state. Search first; never assert a remembered fact you did not
   look up.
2. Log every session (start and end), same agent name.
3. Prefer tier 1 or 2; escalate only when you need the detail.
4. Secrets only via the terminal vault, never `mind_add`, never echoed.
5. Retire a changed single-valued fact (query, invalidate, add the new one).
6. Confirm before destructive actions (dropping a library is CLI and irreversible).
7. If `mind_web` is available, read a page before summarizing it; do not invent its
   contents. Offer to save the useful parts with `mind_add`.

---
MGI-Mind v2.3.0 | Apache-2.0 | Mad God Inc
