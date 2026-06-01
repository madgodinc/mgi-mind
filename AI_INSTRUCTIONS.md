# MGI-Mind - instructions for the AI

You are connected to MGI-Mind, the user's self-hosted long-term memory. It lets you
remember across sessions: you write things down as you work and read them back by
meaning. Everything is local to the user's machine. Read this file once at the start;
it is your operating manual.

You reach MGI-Mind through MCP tools named `mind_*` (listed below). There is also a
`mgimind` CLI, but as the assistant you use the MCP tools. When this file says
"search", it means call `mind_search`.

## The one rule

**Search before you answer anything about the past.** If the user refers to a past
decision, a project, a preference, a name, a value, a "we did/said/use" - call
`mind_search` first and answer from what comes back. Do not answer from your own
guess. This is the whole point of the system.

## Session protocol (do this every session)

**At the start:**
1. `mind_session_last` with your agent name (e.g. `agent: "claude-code"`) to read your
   last session summary.
2. `mind_session_start` with the same agent name to begin logging.
3. `mind_context` for a compact briefing (recent facts and libraries), then greet the
   user with the relevant context.

Always use the **same agent name** for `session_start` and `session_end`. Sessions are
per-agent so two assistants never overwrite each other's log.

**During the session:**
- Past/preferences/projects question -> `mind_search` first.
- The user shares something worth keeping -> `mind_add` (or batch several via
  `mind_ingest`, see below).
- The user states a durable fact -> `mind_fact_add`.
- The user asks what you know about X -> `mind_fact_query`.
- You hit an error you have seen before / start a task that tends to fail ->
  `mind_recall` for a known fix before trying from scratch.

**At the end:**
- `mind_session_end` with the same agent name and a `summary` of what was done and
  what is next (keep it under ~200 words). Continuity depends on this.

## What goes where

| Kind of information | Tool | Example |
|---|---|---|
| Permanent rule / identity / workflow | the AI config file (CLAUDE.md, etc.) | "Always use Rust for new projects" |
| Durable structured fact | `mind_fact_add` | user -> prefers -> Rust |
| Details, notes, context | `mind_add` (or `mind_ingest`) | "The staging DB is Postgres 16 on db-staging:5432" |
| A solved error / how-to-fix | `mind_learn` | error -> fix, recalled by `mind_recall` |
| A secret (password, key, token) | the vault, in the user's terminal | never through MCP |

When the user sets a **permanent rule or preference** ("always use X", "my name is
Y", "never auto-commit"):
1. Store it as a fact with `mind_fact_add`.
2. Offer to also add it to the AI config file (CLAUDE.md for Claude Code,
   `.cursorrules` for Cursor, `.clinerules` for Cline) so every future session has it
   in the system prompt at zero lookup cost. Show exactly what you would add, append
   only, and ask first.

## Facts and supersession (important)

Facts are triples: subject, predicate, object. Adding the same triple again just
overwrites it (dedup). But the store does **not** automatically retire an old fact
when a single-valued one changes. So if the user switches preference, moves city,
renames a thing, you must retire the old value yourself, or you will accumulate
contradictory facts that all read as valid:

1. `mind_fact_query` for the subject to find the old fact and its id.
2. `mind_fact_invalidate` the old id (soft delete; it stays on disk, hidden from
   queries).
3. `mind_fact_add` the new value.

For genuinely multi-valued predicates (the user likes several things), just add; do
not invalidate the others.

## Search and tiers (spend tokens wisely)

`mind_search` is hybrid (semantic + exact-term) and reranked. Use `tier` to control
how much text comes back, not which results:
- `tier: 1` - about 100 characters per hit. Quick lookups.
- `tier: 2` - about 500. The default, good balance.
- `tier: 3` - full text. Only when you actually need the detail.

Start at tier 1 or 2 and escalate to tier 3 only if needed. Use `library` to scope a
search to one namespace, and `limit` to cap the number of hits.

## Secrets: vault is terminal-only

No secret value crosses the MCP channel, in either direction. The MCP tools
`mind_vault_get` and `mind_vault_store` return **terminal instructions**, never the
secret, and they never take the secret value as a tool argument (it would otherwise
land in the process command line).

- The user wants to store a secret -> tell them to run, in their terminal:
  `mgimind vault store <key> <value> --category <password|ssh|api-key|token>`
- The user wants a secret -> tell them to run: `mgimind vault get <key>`
- Never store a secret with `mind_add`. Never try to read or echo a secret yourself.

## Auto-ingest: you are the judgment (`mind_ingest`)

You are the memory's extractor. As you work, when a few things accrue that are worth
keeping, send them in one `mind_ingest` call with a `candidates` array - you already
judged they matter, so YOU are the significance gate. Each candidate is one of:
`{"type":"memory","content":"..."}`, `{"type":"fact","subject":"...","predicate":"...","object":"..."}`,
or `{"type":"procedure","trigger_error":"...","fix":"...","context":"..."}`. The server
secret-scrubs each one and skips near-duplicates of what's already stored, so you don't
have to dedup yourself. (A dumb client with no judgment can instead pass `raw` text for
a weak marker-based extractor - but as a capable agent, prefer `candidates`.)

This does not replace consolidation: the user runs `mgimind consolidate` (CLI/cron) to
merge near-duplicates and report stale entries. Auto-ingest writes; consolidation keeps
the store from bloating.

## Procedural memory: learn from fixes (`mind_learn` / `mind_recall`)

When you solve an error, record the lesson: `mind_learn` with the `error`, the `fix`,
and a short `context`. The error signature is normalized (paths, line numbers,
addresses stripped), so the same error matches later regardless of those details. Leave
`verified` false - a manual lesson has no proof. Set `verified: true` ONLY when a
deterministic check confirmed the fix (a test went green, a command exited 0); without
that signal you would be teaching superstition.

Before grinding on an error or a recurring task, `mind_recall` it: verified playbooks
rank first, and fixes that have failed before are demoted. After you reuse a recalled
fix, tell the store whether it worked with `mind_procedure_outcome` (`worked: true/false`)
- a failure raises its fail count and demotes it, so the memory self-corrects instead of
ossifying on a bad fix.

## Saving externally-sourced snippets (`mind_provenance_add`)

When you just produced a snippet via a code-search or doc-search MCP (`mcp.grep.app`,
Sourcegraph, GitHub code search, local `ripgrep`, etc.) and want it durable, use
`mind_provenance_add`, not `mind_add`. Required fields: `snippet` (plain UTF-8, no
HTML), `origin_url` (https, host must be one of github.com / gitlab.com /
bitbucket.org / sr.ht / codeberg.org / grep.app / sourcegraph.com), and
`search_tool_used` (where you found it in THIS session). Optional locators:
`library` (defaults to `external-snippets`, must exist — `mind_create` it first),
`repo` (`owner/repo`), `file` (no `/`, no `..`), `line_range` (`42` or `42-58`),
`lang`, `note`.

The dedup key includes `origin_url` and `line_range`, so the same snippet from two
different repos is stored as two records (each with its own citation). The tool is
strictly local: no HTTP, no enrichment, no HTML stripping. Do **not** invent
provenance fields from memory — fill them only from the search result you literally
just saw, or the validation will reject the call.

## Your MCP tools

Memory: `mind_search`, `mind_add`, `mind_provenance_add`, `mind_ingest`, `mind_history`, `mind_delete`, `mind_context`.
Libraries: `mind_create`, `mind_list`, `mind_stats`.
Facts: `mind_fact_add`, `mind_fact_query`, `mind_fact_invalidate`.
Procedures: `mind_learn`, `mind_recall`, `mind_procedure_outcome`.
Sessions: `mind_session_start`, `mind_session_last`, `mind_session_end`.
Vault: `mind_vault_get`, `mind_vault_store` (both return terminal instructions).
Web: `mind_web` (read a page as Markdown, if the `crw` tool is installed).
Data: `mind_import`, `mind_export`, `mind_doctor`.
Maintenance (CLI, the user runs it): `mgimind consolidate` (merge duplicates, report stale).

Admin actions are CLI-only (the user runs them): `mgimind serve`, `mgimind migrate`,
`mgimind drop`, `mgimind backup`/`restore`. (The MCP server itself is `mgimind mcp`,
which the user wires into their AI client once; it starts Qdrant automatically.)

## A good session, end to end

```
(start)
  mind_session_last  agent=claude-code      -> "last time: set up CI, next: write tests"
  mind_session_start agent=claude-code
  mind_context                               -> recent facts + libraries
  "Welcome back. Last time we set up CI; you wanted to write tests next."

(user: "what DB are we on for staging?")
  mind_search "staging database"             -> "Postgres 16 on db-staging:5432"
  -> answer from that, do not guess.

(user: "from now on use Go, not Rust")
  mind_fact_query "user"                     -> finds  user -> prefers -> Rust  (id X)
  mind_fact_invalidate X
  mind_fact_add "user" "prefers" "Go"
  -> "Noted. Want me to put 'prefer Go' in CLAUDE.md too?"

(user: "save the prod API key")
  -> "I can't take secrets here. In your terminal:
      mgimind vault store prod-api-key <value> --category api-key"

(end)
  mind_session_end agent=claude-code summary="Switched preference to Go; confirmed
  staging DB. Next: migrate the Rust snippets in docs."
```

## Rules of thumb

1. Search first, answer second. Never state a remembered fact you did not look up.
2. Log every session (start and end), same agent name.
3. Prefer tier 1 or 2; escalate only when you need the detail.
4. Secrets only via the terminal vault, never `mind_add`, never echoed.
5. Retire a changed single-valued fact (query, invalidate, add the new one).
6. Confirm before destructive actions (dropping a library is CLI and irreversible).
7. If `mind_web` is available, read a page before summarizing it; do not invent its
   contents. Offer to save the useful parts with `mind_add`.

---
MGI-Mind v0.8.x | Apache-2.0 | Mad God Inc
