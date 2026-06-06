# I tested my own headline feature and it didn't work

*2026-06-05. About mgi-mind v1.4–v1.6, an integration test I should have written six weeks ago, and what 290 passing unit tests can't tell you.*

---

## TL;DR

I shipped six versions of an AI-agent memory layer over two weeks. v1.4 added a "validity model": predicates have a cardinality (Single / TemporalSingle / Multi), and a duel rule resolves conflicting facts. The duel rule decides who wins, dampens the loser, keeps both for audit. 290 unit tests, ADRs, four critic rounds, two README translations, contributor docs.

Yesterday I tested it on real data: the 35k-fact knowledge graph extracted from two years of my personal notes. Added a conflicting fact. Queried the subject. **Both facts came back as Active.**

The headline feature was broken, and had been since v1.4 with no error or warning.

This post covers why my test suite missed it, why my hand-curated facts caught it in five minutes, and what changes when you stop optimizing for "everything green" and start optimizing for "what a user sees on day one."

If you came for benchmarks: R@5 = 99.2% on LongMemEval-S (e5-base FP16 + reranker, RTX 3090). That number hasn't moved since v0.14.3. The v1.4–v1.6 work changes what memory means, not how it's retrieved. The number stands, and it isn't the point of this post.

---

## What was supposed to happen

Default RAG memory accumulates contradictions like garbage. Log "I use Rust" today, "I use Go" tomorrow, search returns both ranked by cosine similarity. The model has no signal that the new fact supersedes the old one.

The v1.4 fix:

- Every predicate gets a `Cardinality` enum: `Single` (one value at a time, current value wins), `TemporalSingle` (chain, latest is current but history queryable), `Multi` (independent values coexist).
- When a new fact arrives, the duel rule looks at existing facts for the same `(subject, predicate)`. If cardinality forbids coexistence and a conflict exists, it computes confidence scores from cached signals (`dependants_count`, `confirmations_count`, `external_signals`), ranks the new fact against the incumbent, and returns one of: `Flip` (newcomer wins, incumbent dampened), `Contested` (both stay live), `Quarantine` (newcomer too weak, parked for promote-on-repeat).
- Loser gets `status = stale`, `valid_until = now`. Never deleted. Mechanism 1 invariant: future evidence can reverse a duel.

Five months of architecture work come down to one user-facing promise:

> Single-cardinality conflicts auto-resolve. The current value is what comes back.

## What actually happened

I had a `(subject, predicate, object)` test fixture ready: a fresh subject, `Single` cardinality, two distinct objects added back-to-back. I expected the second add to dampen the first. I asked the running MCP server.

```
$ mgimind mcp <<< '{"jsonrpc":"2.0","id":1,"method":"tools/call",
    "params":{"name":"mind_predicate","arguments":
      {"action":"register","predicate":"has_status","cardinality":"Single"}}}'
→ "Registered 'has_status' as Single."  ✓

$ mgimind mcp <<< '{"id":2,...,"name":"mind_fact",
    "arguments":{"action":"add","subject":"x","predicate":"has_status","object":"alpha"}}'
→ "Fact added: x -> has_status -> alpha"  ✓

$ mgimind mcp <<< '{"id":3,...,"name":"mind_fact",
    "arguments":{"action":"add","subject":"x","predicate":"has_status","object":"beta"}}'
→ "Fact added: x -> has_status -> beta"  ✓ ← suspicious, but: maybe it dampened alpha silently?

$ mgimind mcp <<< '{"id":4,...,"name":"mind_fact",
    "arguments":{"action":"query","subject":"x"}}'
→ "x -> has_status -> alpha
   x -> has_status -> beta"  ✗
```

Both Active. No audit event. The duel rule was silent.

## Locating the bug

The 290 unit tests pass. The CLI integration suite (6 tests) passes. The build is warning-free. So either the bug is in something none of them touch, or it's in a place where unit tests and the production read path disagree about reality.

I added two `eprintln!` lines to `knowledge::add_fact`:

```rust
let cardinality = get_cardinality(config, predicate).await?;
eprintln!("[DUEL DEBUG] cardinality={cardinality:?} admits_conflict={}",
          cardinality.admits_conflict());

let existing = if cardinality.admits_conflict() {
    find_facts_by_subject_predicate(config, subject, predicate).await?
} else {
    Vec::new()
};
eprintln!("[DUEL DEBUG] existing.len()={}", existing.len());

let detected = detect_conflict(&existing, object, cardinality);
eprintln!("[DUEL DEBUG] detect_conflict={detected}");
```

Re-ran the reproducer:

```
[DUEL DEBUG] add_fact object="beta" cardinality=Single admits_conflict=true
[DUEL DEBUG] existing.len()=1
[DUEL DEBUG] detect_conflict=true
[DUEL DEBUG] duel outcome=Flip loser=Some("…id of alpha…")
Fact added: x -> has_status -> beta
```

**The duel rule fires correctly.** It detects the conflict, runs the resolver, returns `Flip`, marks alpha as the loser. `dampen_loser(alpha_id)` is called. It writes `status = "stale"` and `valid_until = now` to the loser's payload.

But the query still returns both. So the bug is downstream of `dampen_loser`.

`query_facts` filter:

```rust
let filter = Filter {
    must: vec![Condition::matches("valid", "true".to_string())],
    should: vec![
        Condition::matches_text("subject", query),
        Condition::matches_text("predicate", query),
        Condition::matches_text("object", query),
    ],
    ..Default::default()
};
```

There it is. `valid="true"` is the only state filter. `dampen_loser` writes `status`, not `valid`. The loser remains `valid="true"` (it isn't invalid, it lost a duel, and those are different things in the post-v1.4 model), so it passes the filter and shows up in every query.

The fix is six lines:

```rust
let filter = Filter {
    must: vec![Condition::matches("valid", "true".to_string())],
    must_not: vec![Condition::matches("status", "stale".to_string())],
    should: vec![ /* … */ ],
    ..Default::default()
};
```

End-to-end re-run after the fix:

```
$ mgimind mcp <<< '… add alpha …'        → Fact added
$ mgimind mcp <<< '… add beta (conflict) …' → Fact added
$ mgimind mcp <<< '… query x …'           → "x -> has_status -> beta"   ✓
```

One line of `must_not`. The headline feature finally works.

## Why the test suite didn't catch this

The unit tests cover `detect_conflict`, `resolve_against_existing`, `dampen_loser` independently. Each one has its own happy and sad path. They never compose into one test that calls `add_fact` twice and reads `query_facts` after, because:

- `add_fact` lives behind an MCP boundary; unit tests don't go through it.
- The CLI integration tests use the binary, but the fact-flow tests use `mgimind add` (memories, not facts) and `mgimind search`. No test walked register → add → conflicting add → query through facts.
- I wrote the `query_facts` filter before `status` existed (v1.0 had only the `valid` bool). When I added `status` in v1.4, I updated every write path. I left the reader alone, on the assumption that valid was still the source of truth. The two-state model became a four-state model, but the read path stayed two-state.

The shape of the bug: a feature I added on 2026-05-29 (v1.4 duel rule writes) had no end-to-end test against the read path I added on 2026-04-15 (v1.0 query filter). Each side stayed correct against its own contract. They never met.

## What I did about it

Wrote the integration test I should have written six weeks ago:

```rust
#[test]
fn duel_rule_dampens_loser_on_single_cardinality() {
    // Walk: register cardinality → add winner → add conflicting → query.
    // Assert: only winner returned.
    // Pins the #25 regression so the bug cannot return silently.
}

#[test]
fn multi_cardinality_allows_coexistence() {
    // Regression check: Multi predicates still allow coexistence.
}
```

Both run in 0.2 s against a real Qdrant. The vectorless path needs no embedding model, so CI runs them on every push.

Audited the rest of the read paths for the same omission. Found three more: `list_all_facts`, `list_top_dependants_facts`, `mgimind doctor`'s summary loop. All filtered on `valid="true"` only. Fixed all three.

Wrote a `mgimind migrate-v14 redo-duels` walk. It scans every `(subject, predicate)` cluster, identifies Single/TemporalSingle predicates with > 1 active fact, runs the duel rule across the cluster, dampens losers. This cleans up legacy data from before the read-path fix.

I ran it on my own 35k-fact KG. Output:

```
Found 31 conflict-bearing cluster(s):

  [Single] "Aurora" -> "has_status" (2 active)
    keep      frozen permanently (since 2026-05-29) — hosted on dead brain server
    dampen    active (April 2026 — running on brain server, working on stream)

  [Single] "Mad's HN account MadGodInc" -> "has_status" (2 active)
    keep      hellbanned (confirmed 2026-06-05) — 5 comments invisible to others
    dampen    created 2026-05-29, considered healthy initially

  [TemporalSingle] "mgi-mind" -> "has_version" (7 active)
    keep      v1.6.4 (current as of 2026-06-05)
    supersede v1.5.0 (install-mode profiles + mind_outcome + active re-test)
    supersede v1.4.0 (validity model phase 1: cardinality registry + duel rule)
    supersede v0.14.3 (GPU benchmarks: R@5=99.2%)
    supersede v0.11.0 (quarantine + relevance gate)
    supersede v0.8.0 (first cross-platform Rust stdio-MCP)
    …

Summary: 31 cluster(s) processed, 82 losers cleared.
```

The real opinion changes in my notes (Aurora freezing after my brain server died, my HN account turning out to be hellbanned, two years of mgi-mind release history) all collapsed to the right answer. Mechanism 1 invariant preserved: the losers stay readable via `mind_history` for the temporal cases, and via the audit log for the dampened ones.

For TemporalSingle I added an `EntryStatus::Superseded` variant separate from `Stale`. Same default-hidden behavior, different semantics: `Stale` means "lost a contradiction duel"; `Superseded` means "was correct at its time, a successor took over." A user who asks "when did Aurora freeze?" gets a real history, and doesn't see ghost data in normal queries.

## What 290 unit tests can't tell you

You can have:

- 290 unit tests passing.
- 6 CI workflows green on 3 OSes.
- Four critic rounds of architecture review.
- README translated to three languages.
- ADRs for every major decision.

…and the user-facing version of your headline feature can stay broken for a month and a half without anyone noticing, including you.

What I had built was internal consistency: every piece behaves correctly against its own local contract. What I had not built was external validity: the production read-after-write loop does what the docs say. The two look the same when the build is green.

To detect the difference, act like a first-day user. Open the MCP server you'd ship. Send the calls a real client would send. Read the responses a real client would read. Don't go through the unit-test boundary, don't use a hand-rolled fixture, don't `cargo test`. Type in the thing the readme says will work, and see if it works.

Yesterday, "I'm going to verify the validity model on my real data" sounded like an optional checkpoint. It turned out to be the most valuable test I have written. Every other test in the codebase ran in 0.07 s and confirmed nothing about whether my software does what it claims.

## What I'm not claiming

- The R@5 number didn't change. The duel rule fires at write time; retrieval ranks the resulting facts. If only the winners survive, the search ranks them in the same order as before, against the same evaluation set. This bug fix leaves the 99.2% headline unaffected, and doesn't improve it either.
- I haven't run the STALE benchmark calibration. Issue #16 is open. The architecture changes need their own `R@5 regression < 1.0 pp` gate. The judge-model adapter is unfinished; the dataset wiring needs an API key I don't have yet.
- The four `pub const` weights in the install-mode profiles (`chat-only`, `dev-with-ci`, `multi-tenant`) are starting points, not calibrated. Source comments label them `TODO(phase-4-calibration)`. A real sweep will move them.
- Mac and Windows binaries are still not in v1.5/v1.6; issues #19 and #20 are open. If you'd run mgi-mind on either, please comment in the issue with your use case.

## What you can run today

```sh
# Linux x86_64, ~25 MB stripped:
curl -L https://github.com/madgodinc/mgi-mind/releases/latest/download/mgimind-x86_64-unknown-linux-musl.tar.gz | tar -xz

./mgimind init
./mgimind serve &           # MCP stdio-JSON-RPC server starts

# Register a predicate's cardinality:
./mgimind mcp <<< '{"jsonrpc":"2.0","id":1,"method":"tools/call",
  "params":{"name":"mind_predicate","arguments":
    {"action":"register","predicate":"lives_in","cardinality":"TemporalSingle"}}}'

# Add a fact:
./mgimind mcp <<< '{"jsonrpc":"2.0","id":2,"method":"tools/call",
  "params":{"name":"mind_fact","arguments":
    {"action":"add","subject":"Alice","predicate":"lives_in","object":"Prague"}}}'

# Add a conflict — duel rule resolves it:
./mgimind mcp <<< '{"jsonrpc":"2.0","id":3,"method":"tools/call",
  "params":{"name":"mind_fact","arguments":
    {"action":"add","subject":"Alice","predicate":"lives_in","object":"Dublin"}}}'

# Query — only canonical Dublin:
./mgimind mcp <<< '{"jsonrpc":"2.0","id":4,"method":"tools/call",
  "params":{"name":"mind_fact","arguments":
    {"action":"query","subject":"Alice"}}}'

# Clean up legacy mixed clusters from before v1.7:
./mgimind migrate-v14 redo-duels --apply
```

If you have a pre-v1.7 install with mixed-state clusters, run the walk first. It's idempotent. Dry-run is the default; `--apply` writes.

---

## Appendix: links

- [Issue #25](https://github.com/madgodinc/mgi-mind/issues/25) — bug report with reproducer.
- [PR #26](https://github.com/madgodinc/mgi-mind/pull/26) — fix + read-path audit + integration tests + retroactive walk + `EntryStatus::Superseded`.
- [Issue #27](https://github.com/madgodinc/mgi-mind/issues/27) — pairwise vs cluster duel resolution (follow-up).
- [Source](https://github.com/madgodinc/mgi-mind) | [v1.6.4 release notes](https://github.com/madgodinc/mgi-mind/releases/tag/v1.6.4)
- [Benchmarks folder](https://github.com/madgodinc/mgi-mind/tree/main/benchmarks) — v0.14.3 R@5 = 99.2%; manual-rebuild-2026-06-05 (this story).
- [LongMemEval](https://github.com/xiaowu0162/LongMemEval) — the dataset.

Discussions and issues are how I find out what I should be measuring next.
