# Design: `mind_provenance_add` — provenance-tagged memory ingest

Status: **design only, not implemented**. Branch: `feat/grep-integration`
(retained for history; the tool is no longer about grep.app specifically).
Owner: Mad. Drafted: 2026-06-01. Revised: 2026-06-01 (search-half dropped).

## 0. What changed vs. the previous draft

The previous version of this document proposed two tools, `mind_grep` and
`mind_grep_save`. `mind_grep` was a thin scraper of `https://grep.app/api/search`
— an **internal, unofficial endpoint** of the grep.app site that sits behind
Vercel anti-bot. That coupling is wrong for a long-lived memory tool: we would
be tying the durability of mgi-mind to a third-party service that actively bans
automated access, with no contract and no SLA.

**Decision (Mad, 2026-06-01):** drop the search half entirely. mgi-mind does
**not** do code search. Search is the agent's job, via whichever upstream is
appropriate in the current session: `mcp.grep.app` (the standalone MCP that
*is* engineered around that endpoint and is someone else's maintenance
burden), Sourcegraph, GitHub code search, local `ripgrep`, anything else.

mgi-mind keeps only the half that actually belongs in a memory layer: a
provenance-aware persistence tool that turns a transient external citation
into a durable, dedup-keyed memory record. That tool is renamed:

`mind_grep_save` → **`mind_provenance_add`**

"code" was a lie about scope. Tomorrow we will want to save an RFC quote, a
commit message, or a doc fragment with the same provenance discipline. The
new name says what the tool actually does: *add a memory with a mandatory
source citation*.

## 1. Motivation

`mind_add` accepts free text with no required provenance. That is correct for
the user's own notes. It is wrong for anything the agent **found somewhere**:
without a citation, six months later we cannot tell whether a stored snippet
is a quote, a paraphrase, or hallucination.

`mind_provenance_add` is the same persistence path as `mind_add`, but with the
provenance fields **promoted from optional to required** and validated. The
tool exists to make it cheap to do the right thing (cite-then-save) and
impossible to do the wrong thing (save without a source URL).

## 2. Tool surface

```jsonc
{
  "name": "mind_provenance_add",
  "description": "Persist an externally-sourced snippet (code, doc, RFC quote, commit message, etc.) into mgi-mind with a mandatory provenance citation. The agent supplies the snippet AS PLAIN UTF-8 — no HTML, no markup. Call this ONLY when the snippet was just produced by a code-search or doc-search MCP in the same session; do NOT fill provenance fields from memory.",
  "inputSchema": {
    "type": "object",
    "properties": {
      "library":          { "type": "string", "default": "external-snippets", "description": "Target library. Must exist (create with mind_create)." },
      "snippet":          { "type": "string", "description": "Raw text to store. Plain UTF-8. Must NOT contain HTML tags; the agent is responsible for stripping markup upstream." },
      "origin_url":       { "type": "string", "description": "https:// URL the snippet was lifted from. Host must be in the allowlist (see §4)." },
      "repo":             { "type": "string", "description": "Optional owner/repo when the source is a code host. Regex: ^[\\w.-]+/[\\w.-]+$." },
      "file":             { "type": "string", "description": "Optional path inside the repo. No leading '/', no '..' segments." },
      "line_range":       { "type": "string", "description": "Optional line range, e.g. \"42\" or \"42-58\". Regex: ^\\d+(-\\d+)?$." },
      "lang":             { "type": "string", "description": "Optional language tag (free string). Unknown values are logged but accepted." },
      "search_tool_used": { "type": "string", "description": "Identifier of the search source the agent used in THIS session, e.g. \"mcp.grep.app\", \"sourcegraph\", \"github code search\", \"local ripgrep\". REQUIRED. Empty rejects with 'provenance source unknown, use mind_add instead'." },
      "note":             { "type": "string", "description": "Optional one-liner the agent attaches (why this is worth keeping)." }
    },
    "required": ["snippet", "origin_url", "search_tool_used"]
  }
}
```

**Returns:** `"Saved 1 chunk to 'external-snippets' (id: <uuid>)"` on insert,
or `"Already present in 'external-snippets' (id: <uuid>)"` on dedup hit.

The description doubles as the social contract: it tells the agent *when* to
call this tool (right after a code-search MCP in the same session) and
explicitly forbids fabricating provenance from memory. The validation in §4
is the backstop, not the only line of defense.

## 3. Storage shape

`mind_provenance_add` writes through the existing `crate::storage::add_memory`
path. No new payload schema, no new collection type.

The `content` field that gets embedded looks like:

```
[external] <origin_url>
repo: <owner/repo>            (omitted if absent)
file: <path>                  (omitted if absent)
lines: <line_range>           (omitted if absent)
lang: <lang>                  (omitted if absent)
source: <search_tool_used>

<snippet body, verbatim, plain UTF-8>

note: <agent-supplied; line omitted entirely if empty>
```

Notes:

- **No `saved:` timestamp in the embedded content.** A wall-clock timestamp
  in the embedded text destroys UUIDv5 stability and therefore dedup. mgi-mind
  already records insert time in the existing per-memory metadata; that is
  enough. If a future use case actually needs an explicit ingest timestamp
  surfaced on the record, it goes in the payload, not the embedded content.
- The `source:` line carries `search_tool_used` so a later
  `mind_search "from sourcegraph"` style query works without a new payload
  field.

## 4. Validation

All checks run in Rust **before** `add_memory` is called. Each failure
returns a clear error string naming the offending field.

| Field              | Rule                                                                                                    |
|--------------------|---------------------------------------------------------------------------------------------------------|
| `snippet`          | Non-empty after trim. No NUL bytes.                                                                     |
| `origin_url`       | Parses as URL. Scheme **must** be `https`. Host **must** be in the allowlist below. Otherwise: reject.  |
| `repo`             | If present: matches `^[\w.-]+/[\w.-]+$`. Otherwise: reject.                                             |
| `file`             | If present: must not start with `/`; must not contain a `..` path segment. Otherwise: reject.           |
| `line_range`       | If present: matches `^\d+(-\d+)?$`. Otherwise: reject.                                                  |
| `lang`             | Optional, free string. Unknown values are accepted and logged at `info` for future allowlist tightening. |
| `search_tool_used` | Required. Empty / whitespace-only → reject with `"provenance source unknown, use mind_add instead"`.    |

**`origin_url` host allowlist** (case-insensitive, exact host match — no
subdomain wildcards in v1):

- `github.com`
- `gitlab.com`
- `bitbucket.org`
- `sr.ht`
- `codeberg.org`
- `grep.app`
- `sourcegraph.com`

A non-https URL, or a host outside this list, is rejected with
`"origin_url host '<host>' is not in the provenance allowlist; widen the list in a follow-up PR if this is intentional"`.
The list is deliberately small at v1 and lives in a single `const` in
`src/provenance.rs` so widening it is a one-line PR with explicit review.

The existing `secrets::scan` runs on every `add_memory`. Snippets that trip
it are rejected with the standard error. No new bypass.

## 5. Dedup — identity key

This is the load-bearing change vs. the previous draft.

The deterministic memory id is:

```
uuid_v5(
    NAMESPACE_MGI_MIND,
    library || "\0" || content || "\0" || origin_url || "\0" || line_range_or_empty
)
```

Where:

- `NAMESPACE_MGI_MIND` is the project-wide UUIDv5 namespace already used
  elsewhere.
- `content` is the exact embedded string from §3 (which, crucially, contains
  no timestamp — see §3 note).
- `origin_url` is the normalized input.
- `line_range_or_empty` is the input `line_range` if present, else the empty
  string.

**Why provenance is in the key:** the same snippet legitimately exists in
hundreds of repos (think Apache-2.0 boilerplate, ubiquitous helper functions,
copy-pasted error patterns). The previous plan keyed only on
`library + content`, which collapses all those repos into one memory and
silently throws away the provenance of all but the first ingest.
Provenance-in-the-key means:

- Same `(snippet, origin_url, line_range)` saved twice → one record. Idempotent.
- Same snippet from two different repos → two records, each with its own
  citation. This is the intended behaviour.
- Same snippet, same repo, different line range → two records (the line
  range is part of the citation; an agent that wants to dedup these is
  asking for semantic dedup, which §8.4 explicitly rules out).

Dedup is **exact-key only**. No fuzzy / near-duplicate detection.

## 6. Module layout

New file: `src/provenance.rs` (~150 LOC). Public surface:

```rust
pub struct ProvenanceInput<'a> {
    pub library: &'a str,
    pub snippet: &'a str,
    pub origin_url: &'a str,
    pub repo: Option<&'a str>,
    pub file: Option<&'a str>,
    pub line_range: Option<&'a str>,
    pub lang: Option<&'a str>,
    pub search_tool_used: &'a str,
    pub note: Option<&'a str>,
}

pub fn validate(input: &ProvenanceInput<'_>) -> Result<(), ProvenanceError>;
pub fn format_content(input: &ProvenanceInput<'_>) -> String;
pub fn dedup_id(
    library: &str,
    content: &str,
    origin_url: &str,
    line_range: Option<&str>,
) -> uuid::Uuid;
```

Wiring:

- `mcp.rs::dispatch`: one new arm, `"mind_provenance_add"`.
- `mcp.rs::tool_definitions`: one new schema entry. Bump the test
  `exposes_all_25_tools` to `26`.
- `Cargo.toml`: **no new deps.** `uuid` (with `v5`), `url`, and `regex` are
  already in the tree (or trivially available — `regex` is the only one to
  verify in `Cargo.lock`).

No CLI subcommand in v1. The CLI surface is settled in a follow-up if
demanded; the MCP tool is the contract.

## 7. Concept fit with mgi-mind

mgi-mind already accepts agent-curated text via `mind_add` and `mind_ingest`.
`mind_provenance_add` is the strict variant of that path: same backend, same
embeddings, same recall — but with provenance as a load-bearing input rather
than an afterthought. It is *less* tool surface than the previous draft, not
more.

## 8. Explicit non-goals

1. **Does not scrape grep.app** (or any other site). No HTTP client added.
2. **Does not go to the network at all.** Pure local validation + `add_memory`.
3. **Does not enrich provenance.** No fetch of file content from `origin_url`,
   no resolution of `HEAD` to a commit SHA, no metadata enrichment. Garbage
   in, garbage in — but the validation in §4 catches most garbage.
4. **Does not dedup by similarity.** Only by the exact UUIDv5 key from §5.
5. **Does not strip HTML / markup.** The contract is plain UTF-8 in; if the
   agent passes `<mark>` tags or HTML entities, that is the agent's bug. The
   previous draft included a hand-rolled de-tagger; that code, and its
   attendant footguns (`Vec<T>` looking like a tag, `a < b && c > d`, nested
   `<mark>` from regex highlights, etc.) is **removed from scope**.
6. **No new vault category.** `vault` remains encrypted-secrets-only.
7. **No new MCP method.** Lives inside the existing `tools/call` dispatcher;
   nothing in the JSON-RPC surface changes.
8. **No corpus mirroring.** One snippet per explicit call; never a "save all
   hits" mode.

## 9. Test plan

### 9.1 Unit (in `src/provenance.rs`, `#[cfg(test)]`)

Validation:

- `origin_url_https_only` — `http://github.com/...` → reject.
- `origin_url_host_allowlist_accepts_github` — `https://github.com/...` → ok.
- `origin_url_host_allowlist_rejects_random` — `https://evil.example.com/...`
  → reject with the allowlist error string.
- `origin_url_host_allowlist_is_case_insensitive` — `GitHub.com` → ok.
- `repo_regex_accepts_owner_repo` — `BurntSushi/ripgrep` → ok.
- `repo_regex_rejects_three_segments` — `a/b/c` → reject.
- `file_rejects_absolute` — `file = "/etc/passwd"` → reject.
- `file_rejects_path_traversal` — `file = "src/../../etc/passwd"` → reject.
- `file_rejects_path_traversal_classic` — `file = "../../etc/passwd"` → reject.
- `file_accepts_normal_path` — `crates/regex/src/util.rs` → ok.
- `line_range_accepts_single` — `42` → ok.
- `line_range_accepts_range` — `42-58` → ok.
- `line_range_rejects_garbage` — `42-` / `abc` / `1,2` → reject.
- `search_tool_used_required` — empty → reject with the exact
  `"provenance source unknown, use mind_add instead"` string.
- `search_tool_used_whitespace_only_rejected` — `"   "` → reject.

Content formatting:

- `format_omits_absent_fields` — missing `repo` / `file` / `lines` / `lang` /
  `note` ⇒ those lines absent from the embedded content (not present as
  empty lines).
- `format_has_no_timestamp` — the embedded string contains no ISO timestamp
  and is byte-stable across two calls with identical input.
- `format_includes_search_tool_used` — e.g. `source: sourcegraph` line
  present.

Dedup id:

- `dedup_id_stable_across_calls` — same inputs ⇒ same UUID.
- `dedup_id_changes_with_origin_url` — same snippet, two URLs ⇒ two UUIDs.
- `dedup_id_changes_with_line_range` — same snippet, same URL, different
  ranges ⇒ two UUIDs.
- `dedup_id_is_v5_not_v4` — version nibble check.

### 9.2 MCP-level (extend `src/mcp.rs::tests`)

- `tools_list_returns_26` — replaces the current 25-count assertion.
- `mind_provenance_add_rejects_missing_snippet`.
- `mind_provenance_add_rejects_missing_origin_url`.
- `mind_provenance_add_rejects_missing_search_tool_used`.
- `mind_provenance_add_rejects_empty_search_tool_used` — empty string in a
  present field, exercising the "use mind_add instead" message.
- `mind_provenance_add_rejects_http_url`.
- `mind_provenance_add_rejects_off_allowlist_host`.
- `mind_provenance_add_rejects_path_traversal_in_file`.

### 9.3 Integration (extend `tests/cli_integration.rs`)

Gated on `MGIMIND_IT_QDRANT` like existing tests. End-to-end through real
storage:

- `provenance_round_trip_through_add_memory` — valid call → `mind_search` for
  a token in the snippet returns the record with the expected `[external]`
  header.
- `provenance_dedup_same_inputs_inserts_once` — fire the same call twice;
  the second returns the "Already present" message and the library size
  grows by exactly 1.
- `provenance_same_snippet_two_urls_inserts_twice` — same snippet, two
  different allowlisted URLs; library grows by 2.

### 9.4 Doctor

`mind_doctor --fix` creates the default `external-snippets` library if
missing, parallel to other library-state healing. One new branch in
`run_doctor` plus one assertion in the existing doctor integration test.

## 10. Open questions

1. **Default library name.** `external-snippets` is the proposal;
   `provenance` and `cited` are alternatives. Pick before merging
   implementation.
2. **Allowlist widening policy.** Stack Overflow URLs are an obvious next
   request (`stackoverflow.com`). Deferred: if/when an agent actually needs
   it, widen with explicit review. v1 ships the seven hosts above.
3. **Should `lang` get an allowlist too?** Right now: no — agents may use
   non-obvious tags (`gleam`, `roc`, `mlir`). Log unknowns at `info`; if a
   pattern of typos emerges, tighten later.

## 11. Out of scope for this branch

Implementation. This branch ships **only** this document. Implementation
follows in a separate PR.

## 12. Rough effort estimate (for the implementation PR)

| Slice                                                | Estimate     |
|------------------------------------------------------|--------------|
| `src/provenance.rs` (validation + format + dedup id) | 0.25 day     |
| MCP wiring + schema entry + tool count bump          | 0.1 day      |
| Unit tests (validation, format, dedup id)            | 0.25 day     |
| MCP-level rejection tests                            | 0.1 day      |
| Integration tests + doctor branch                    | 0.2 day      |
| Docs (README, CHANGELOG)                             | 0.1 day      |
| **Total**                                            | **~0.5–1 day** |

Down from ~2.5 days in the previous draft: no HTTP client, no rate-limit
logic, no anti-bot workarounds, no JSON parsing of grep.app responses, no
de-tagger.
