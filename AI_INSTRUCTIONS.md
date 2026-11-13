# MGI-Mind: instructions for the AI

You are connected to MGI-Mind, the user's self-hosted long-term memory. It lets
you remember across sessions: you write things down as you work and read them back
by meaning. Everything is local to the user's machine. Read this file once at the
start; it is your operating manual.

You reach MGI-Mind through MCP tools named `mind_*` (listed at the end). There is
also a `mgimind` CLI, but as the assistant you use the MCP tools. When this file
says "search", it means call `mind_search` (or `mind_recall_all`).

If `mgimind` is not installed yet, or the `mind_*` MCP tools are not available in
this session, start with the next section. Once it is installed and wired in,
the rest of this file (starting at "The one rule") is your operating manual for
every session after that.

## Installing mgi-mind

This section is for an agent that has been asked to install mgi-mind for a user,
starting from nothing but this file (or the repository URL). Work through it in
order: decide the target, install, verify, wire it into the client, then set up
the client's own rules file so future sessions pick up the protocol below
without being told again (this file is not auto-loaded; nothing else here is).

### 1. Decide the target

- **Personal PC** (Windows, macOS, or Linux desktop): the default when the user
  is asking on their own machine and says things like "set this up for me" or
  "install mgi-mind".
- **Server** (always-on Linux box, multiple agents or machines sharing one
  brain, "VPS", "remote"): when the user says so, or you are already working
  over SSH on a machine that is not an interactive desktop.
- **Docker**: when the user already runs things in containers, asks for "a
  container", or wants zero local Rust/Qdrant footprint to manage.

Ask the user only if none of these fit obviously; an install writes to disk and
downloads several hundred MB, so guessing wrong is wasteful, not catastrophic -
prefer asking over guessing when genuinely unsure.

### 2. PC install

One command installs the binary and runs setup:

**Linux / macOS:**

```bash
curl -fsSL https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.sh | sh
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/madgodinc/mgi-mind/main/install.ps1 | iex
```

Installer flags, as environment variables before `install.sh` (verified in the
script's own header comment):

- `INSTALL_DIR` - where the binary goes (default `~/.local/bin`).
- `MGIMIND_TAG` - a specific release tag instead of `latest`.
- `SKIP_DOCTOR` - set non-empty to skip the model/Qdrant/ONNX download at the
  end; run `mgimind doctor --fix` yourself later.
- `MGIMIND_NO_PROFILE` - set non-empty to stop the installer from appending a
  PATH line to the shell profile (macOS zsh does not include `~/.local/bin` on
  PATH by default, so the installer does this unless told not to).

`install.ps1` takes the same three as PowerShell parameters instead: `-InstallDir`
(default `$env:LOCALAPPDATA\Programs\mgimind`), `-Tag` (default `latest`),
`-SkipDoctor` (switch).

Manual path, with no installer script: download the release archive for the OS
from `https://github.com/madgodinc/mgi-mind/releases/latest`, put the `mgimind`
(or `mgimind.exe`) binary on PATH, then:

```bash
mgimind init
mgimind doctor --fix
```

Disk/network cost: `doctor --fix` downloads the embedding model
(multilingual-e5-base, quantized ONNX, ~270 MB) and the reranker
(bge-reranker-base, quantized ONNX, ~280 MB) into `~/mgimind/` (or `%USERPROFILE%
\mgimind` on Windows, or wherever `MGIMIND_HOME` points), plus the Qdrant binary
and ONNX Runtime, which land next to the `mgimind` binary itself - roughly 550 MB
total the first time.

### 3. Verify

Run `mgimind doctor`. A healthy install ends with the line `All checks passed.`
(every check printed `[OK]`, `[--]`, or `[INFO]`, never `[FAIL]`). If anything is
`[FAIL]`, run `mgimind doctor --fix` and check again.

Then a smoke test, straight from the CLI:

```bash
mgimind create smoke-test
mgimind add smoke-test "the install smoke test works"
mgimind search "install smoke test" --library smoke-test
```

The search should return the memory you just added. Drop the throwaway library
once satisfied: `mgimind drop smoke-test`.

### 4. Wire it into the client

Use the full, absolute path to the installed binary in every config below (from
`which mgimind` / `Get-Command mgimind`, or the install directory the installer
printed). A bare `mgimind` depends on the client process having picked up the
PATH change, which a freshly-installed session often has not.

**Claude Code** (verified: this is the exact line both installers print at the
end of a successful run):

```bash
claude mcp add mgimind -- /absolute/path/to/mgimind mcp
```

**Generic stdio MCP client** - every client below ultimately wants this shape;
only the file name and the key path around `mcpServers` differ:

```json
{
  "mcpServers": {
    "mgimind": {
      "command": "/absolute/path/to/mgimind",
      "args": ["mcp"]
    }
  }
}
```

The following file locations are each client's own published convention, not
something mgi-mind's code can verify from inside this repository - if one has
moved since this was written, use the client's current docs instead:

- **Claude Desktop**: `claude_desktop_config.json` (macOS:
  `~/Library/Application Support/Claude/claude_desktop_config.json`; Windows:
  `%APPDATA%\Claude\claude_desktop_config.json`), same `mcpServers` shape above.
- **Cursor**: `.cursor/mcp.json` in the project (or the global `~/.cursor/mcp.json`),
  same `mcpServers` shape.
- **Cline**: `cline_mcp_settings.json` (inside the extension's VS Code settings
  directory), same `mcpServers` shape.
- **Codex CLI**: its MCP config is TOML, not JSON, typically
  `~/.codex/config.toml` with a `[mcp_servers.mgimind]` table holding `command`
  and `args`. Confirm the exact table name against `codex mcp add` or the
  installed Codex CLI's own `--help` before trusting this blindly.
- **Gemini CLI / Antigravity**: `~/.gemini/settings.json` (or the project's own
  `.gemini/settings.json`), with an `mcpServers` object in the same shape.

### 5. Load the behaviour protocol into the client's rules file

AI_INSTRUCTIONS.md is not auto-loaded by any client; wiring in the MCP server
only gives the tools, not the habit of using them. Before finishing the install,
append (never overwrite) a short pointer to this file's protocol into whichever
rules file the client reads every session:

- Claude Code / Claude Desktop: `CLAUDE.md` (project root, or `~/.claude/CLAUDE.md`
  for every project).
- Cursor: `.cursorrules` or `.cursor/rules/*.mdc`.
- Cline: `.clinerules`.
- Codex CLI and other generic agents: `AGENTS.md`.
- Gemini CLI: `GEMINI.md`.

A short pointer is enough, for example:

```
## Persistent memory (mgi-mind)

This project has mgi-mind as long-term memory (MCP tools named `mind_*`).
Before acting on anything about this user's projects, people, environment or
past decisions, call `mind_search` first and treat your own recollection as a
draft to verify. See AI_INSTRUCTIONS.md in the mgi-mind repository for the full
protocol (search triggers, writing things down, the vault, multi-agent use).
```

### 6. Server mode (Linux, systemd, remote agents)

Install the same way as the PC install above (`install.sh`). The differences
are how it is kept running and how other machines reach it, not the install
itself.

Expose the tool surface to other machines with `serve-http` (verified against
`mgimind serve-http --help` and `src/http_api.rs`):

```bash
mgimind serve-http --host 0.0.0.0 --port 8765 \
  --agent-token "alice:TOKEN1" \
  --agent-token "bob:TOKEN2:project-a,project-b"
```

- `--host` defaults to `127.0.0.1` (loopback only, no remote access). Binding
  anything else is refused unless at least one `--agent-token` is given, so an
  accidental open brain cannot happen silently.
- `--agent-token NAME:TOKEN[:lib1,lib2]` is repeatable. A caller's identity is
  derived from its bearer token (not from a self-asserted header). The optional
  `:lib1,lib2` suffix scopes that token to only those libraries (it cannot read
  or write any other library, or reach facts/procedures/quarantine/sessions at
  all); omit it for full access.
- The exposed surface is a fixed allowlist, not the whole MCP tool set: memory
  search/browse/recall/add/ingest/by-agent, fact add/query/invalidate/contested,
  procedure learn/recall, library create/list, quarantine list/promote, memory
  restore/archive/delete/edit, session start/end/last/context, stats, audit,
  kv get/set, should-search. Destructive and bulk operations are not exposed.
- `write_quota_per_min` in `config.json` (default 600) caps writes per author
  per rolling 60 seconds, so one runaway agent cannot flood the shared store.
  Set it to `0` to disable the cap.

Keeping Qdrant running: `mgimind serve-http` brings up the bundled Qdrant itself
on first use (the same path `mgimind mcp` uses), so no separate step is strictly
required. For a long-lived server, supervise `serve-http` itself with your
process manager so it (and the Qdrant child it starts) come back after a reboot
or crash. A minimal systemd unit (not shipped in the repository; write your own
from this skeleton, verified flags only):

```ini
[Unit]
Description=mgi-mind HTTP tool surface
After=network.target

[Service]
Type=simple
Environment=MGIMIND_HOME=/var/lib/mgimind
EnvironmentFile=/etc/mgimind/tokens.env
ExecStart=/opt/mgimind/mgimind serve-http --host 0.0.0.0 --port 8765 --agent-token "${AGENT_TOKEN}"
Restart=on-failure
User=mgimind

[Install]
WantedBy=multi-user.target
```

Put the actual token in `/etc/mgimind/tokens.env` (`AGENT_TOKEN=name:secret`),
never directly in the unit file, which is often world-readable.

Remote agents then connect one of two ways:

- **MCP over SSH** (no port opened at all - the safest option when every agent
  can already SSH into the box): point the client's MCP config at `ssh` as the
  command, e.g. `{"command": "ssh", "args": ["user@host", "/opt/mgimind/mgimind", "mcp"]}`.
  This is a generic SSH+stdio pattern, not special mgi-mind wiring.
- **HTTP against `serve-http`**, with a bearer token as above, when agents
  cannot SSH in directly.

### 7. Docker

The repository's own `Dockerfile` bakes the binary, bundled Qdrant, ONNX Runtime
and both default models into one image, so running it downloads nothing further:

```bash
docker run -p 8765:8765 madgodinc/mgi-mind
```

It prints a bearer token at startup if `MGIMIND_TOKEN` is not set. Pin a stable
token and persist memory across restarts with a volume:

```bash
docker run -p 8765:8765 -e MGIMIND_TOKEN=your-token -v mgimind-data:/data madgodinc/mgi-mind
```

Verified from `docker/entrypoint.sh`: the default entrypoint runs exactly
`mgimind serve-http --host 0.0.0.0 --port 8765 --agent-token "user:$TOKEN"`
internally, so everything in the server-mode section above about
`--agent-token` and the exposed surface applies to clients connecting to this
container too. Pass your own command (`docker run madgodinc/mgi-mind doctor`)
to do something other than serve.

### 8. Troubleshooting

- **"Model not found... run doctor --fix"**: the model is missing under
  `~/mgimind/models/`. Run `mgimind doctor --fix`.
- **A tool (or `mind_doctor`) fails right after install**: run `mgimind doctor`
  first; it reports exactly what is missing (Qdrant not running, a model not
  downloaded, ONNX Runtime absent) and `--fix` downloads what it can.
- **ONNX Runtime / Qdrant binary reported as downloaded but missing**: almost
  always antivirus or OS quarantine eating the file right after download.
  `mgimind doctor` prints a quarantine hint when this happens; allow the
  `mgimind` binary and its data directory in the AV, then re-run `doctor --fix`.
- **Windows SmartScreen** warns on the unsigned `mgimind.exe` ("Windows
  protected your PC"): choose More info -> Run anyway.
- **macOS Gatekeeper quarantine** on a binary downloaded through a browser
  (not the installer, which clears it itself): `xattr -d com.apple.quarantine
  /path/to/mgimind`.
- **Qdrant port already in use**, or a second mgimind install on the same
  machine: set a different `qdrant_port` (and `qdrant_http_port` if it does not
  follow the default `qdrant_port - 1` pairing) in `config.json`, or point
  `MGIMIND_HOME` at a separate directory for a fully independent brain.
- **Windows: `mgimind restore` fails syncing `newest_clocks.json` (access
  denied / os error 5)**: Qdrant below 1.19.2 cannot restore a snapshot on
  Windows. A fresh `doctor --fix` always pulls 1.19.2 or newer (pinned in the
  binary); this only bites an old cached Qdrant binary, so re-run
  `doctor --fix` to replace it.
- **Windows: Qdrant fails with "the system cannot find the specified path"
  while creating a collection, deep inside a long `MGIMIND_HOME` path**: this is
  Windows's ~260-character path limit, not a mgi-mind bug. Use a shorter
  `MGIMIND_HOME` (closer to the drive root) if this happens.
- **Dimension mismatch warning** after changing `model_name`: re-embed with
  `mgimind migrate`.
- **Never ask the user to paste a secret (password, API key, token) into the
  chat so you can store it.** The vault is terminal-only by design: have the
  user run `mgimind vault store <key> <value>` themselves in their own
  terminal. The `mind_vault_*` MCP tools only return instructions; they never
  read or write the secret value over the MCP or HTTP channel.

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
- You are about to start a kind of work the user has house rules for ->
  `mind_skill(action="match", task="...")` BEFORE you begin, not after a correction.

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
| The house way of doing a kind of work | `mind_skill(action="set")` | "CSS animation: transform and opacity only" |
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

## Skills: the house way (`mind_skill`)

A procedure is reactive: something broke and you want the fix that worked. A skill
is the other half. It is how this user wants a kind of work done, and it is worth
nothing if you read it after the fact, so call `mind_skill(action="match",
task="<what you are about to do>")` before starting non-trivial work. The task text
is matched against each skill's trigger, semantically and lexically, so "build a
hover animation" finds a skill whose trigger says "CSS transitions".

`mind_context` lists the catalogue: names and one-line triggers, marked `*` when a
skill has been verified. Treat that list as the index. Pull a body with
`action="match"` or `action="show"` when it applies.

When the user states how they want work done from now on, write it:
`mind_skill(action="set", name="css-motion", when="CSS animation or transition
work", body="...")`. The name is the identity, so correcting a skill later is a
re-write of the same name, never a second copy that disagrees with the first.

After you follow one, close the loop with `action="outcome"`: `worked=false` when
it led you wrong (it sinks in the ranking), `verify=true` only when a
deterministic signal says the result was right.

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
but are deprecated and slated for removal in v2.0. Since v2.8.1 they are also hidden
from `tools/list` by default (30 tools advertised instead of 45), so a client built
against this file should not expect to see them listed; calling one directly by name
still dispatches. Set `MGIMIND_LIST_DEPRECATED=1` in the server's environment to list
them again.

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

Facts: `mind_fact` with `action` add / query / invalidate / restore.
`mind_predicate` for cardinality (single / temporal-single / multi).
Invalidate is final: re-adding the same triple afterward, even one that
differs only in case or whitespace, refuses (verdict `invalidated`, with the
invalidated fact's id) instead of reviving it - use `action=restore` on that
id to bring it back. Re-adding a value that lost a duel within the last
`flip_back_window_hours` (default 24) lands `contested` rather than flipping
the winner back. `action=add` takes an optional `valid_from` (when the fact
became true in the world, as opposed to when it was recorded);
`action=query` with `as_of` takes an optional `world_time=true` to resolve
against `valid_from` instead of the default record-time axis.

Skills: `mind_skill` with `action` match / set / list / show / remove / outcome.
Match before the work; procedures answer after the error.

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
`mgimind drop`, `mgimind backup` / `restore`, `mgimind relibrary` (move memories
between libraries — a destructive, regex-matched, cross-library operation; dry-run
by default). The MCP server itself is `mgimind mcp`, which the user wires into
their AI client once; it starts Qdrant automatically.

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
MGI-Mind v2.10.0 | Apache-2.0 | Mad God Inc
