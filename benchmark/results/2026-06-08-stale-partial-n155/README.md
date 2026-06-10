# STALE partial run — N=155, 2026-06-08

Raw per-scenario verdicts behind the STALE table in `BENCHMARKS.md`. Saved here
so the published number is reproducible from data rather than a hand-typed
figure.

## What these files are

Six JSONL files, one verdict per line, 155 scenarios total (47 T1, 108 T2). Each
line is one scenario's final grade:

```json
{"state_resolution": true, "premise_resistance": true,
 "implicit_policy_adaptation": true, "type": "T1", "uid": "..."}
```

- `t1a_out.jsonl`, `t1b_out.jsonl` — T1 (co-referential) scenarios.
- `t2_1_out.jsonl` .. `t2_4_out.jsonl` — T2 (propagated) scenarios.

These are the judge's final SR/PR/IPA booleans only. The intermediate
`(query, system_answer, expected_belief)` triples that went to the judge are
**not** in these files; reproducing those needs a fresh run with prompt logging.

## How the published number is computed

STALE Overall is **scenario-level**: a scenario counts as a pass only if all
three of SR, PR, IPA are true (not the per-cell average). Per type, then macro:

```python
import json, glob
rows = [json.loads(l) for f in sorted(glob.glob("*_out.jsonl"))
        for l in open(f) if l.strip()]
def all3(rs):
    return round(100 * sum(r["state_resolution"] and r["premise_resistance"]
                           and r["implicit_policy_adaptation"] for r in rs) / len(rs))
t1 = [r for r in rows if r["type"] == "T1"]   # N=47  -> 38%
t2 = [r for r in rows if r["type"] == "T2"]   # N=108 -> 26%
overall_macro = round((all3(t1) + all3(t2)) / 2)  # 32%
```

This reproduces the BENCHMARKS table exactly: T1 38%, T2 26%, Overall ~32% macro.
(The per-cell average is higher, ~44%, because a scenario can pass some cells and
fail others; STALE scores the whole scenario, so all-3-pass is the correct, and
stricter, figure.)

## Provenance and caveats

- **The harness lived on a branch, not main.** This run was produced by
  `bench_stale::run` on `stale-extraction-optimization` (commit `0e48b75`), which
  has the working cloud-extract → answerer → judge pipeline. On `main`,
  `bench_stale::run` is still a scaffold. So this number is real but was **not**
  produced by the code currently on main.
- **Config:** `--llm-extract --backbone gemini-flash-latest --judge
  gemini-flash-latest --haystack reduced --window 2 --focused`, cross-axis
  adjudicator on. LLM-in-the-loop end to end (extract + answer + judge), not
  zero-API.
- **Partial:** 155 of 400. Stopped on judge rate limits.
- **Possible confound:** the duel rule (the mechanism STALE tests) was silently
  broken from v1.4 to v1.6 and fixed close to this run. This number may have been
  taken before supersession worked end-to-end on every scenario, so a re-run on
  the fixed mechanism could move it. A small re-run through committed code is the
  way to settle that.

See `BENCHMARKS.md` (STALE section) for the table and the full caveat list.
