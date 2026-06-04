# ADR 0003 — §10 q5 three guarantees on the background loop

**Status:** accepted.
**Date:** 2026-06-04.

## Decision

`spawn_background_retest_loop` provides three hard guarantees in
production code, each tested at the registry / scheduling level:

- **(a)** Never concurrent with an MCP tool call.
- **(b)** Hard per-tick cap (assert at end of walk).
- **(c)** Load-aware cadence (back off on `/proc/loadavg` pressure).

## Context

Validity-model synthesis §3 mechanism 2 introduces active re-test:
top-N entrenched facts get periodically re-evaluated against the
current memory centroid. This closes the gap that mechanism 2's
retrieval-triggered doubt window leaves — facts never retrieved
never enter doubt.

§10 question 5 sets the operational constraints. Without them
"background loop" is a euphemism for "loop that starves the
foreground path randomly".

## Guarantee (a) — never concurrent with MCP

A `BusyGuard` raises a process-global `AtomicBool` MCP_BUSY:

```rust
pub struct BusyGuard;

impl BusyGuard {
    pub fn new() -> Self {
        MCP_BUSY.store(true, Ordering::Release);
        BusyGuard
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        MCP_BUSY.store(false, Ordering::Release);
    }
}
```

Every MCP tool dispatch in `mcp::call_tool` opens with
`let _g = BusyGuard::new();`. RAII Drop on panic too — covers the
panicking-tool path.

The loop checks `is_mcp_busy()` at two points:

1. **Outer wake.** After every `tokio::time::sleep`, before doing
   any work.
2. **Between facts.** Inside the walk, after each fact is
   processed.

The second check is what closes the "tool call started mid-tick"
hole. Without it a tool call landing 1 ms after the busy check at
outer wake would race the walk for several Qdrant round-trips
before yielding.

## Guarantee (b) — hard per-tick cap

`BACKGROUND_PER_TICK_CAP = 50` is the limit. Two places enforce it:

1. **At selection.** `select_retest_candidates(_, cap)` returns at
   most `cap` ids — drains flagged-for-doubt registry first, tops
   up with high-dependants facts.
2. **At end of walk.** `assert!(n_processed <= BACKGROUND_PER_TICK_CAP)`.

The assert is intentional. If a future refactor breaks the cap,
the background task panics. The task auto-restarts via the outer
scheduling, so the loop survives — but the failure is loud, not
silent. Worse than wrong is "background loop scans 10× more work
than expected for three months before anyone notices".

## Guarantee (c) — load-aware cadence

`loadavg_multiplier()` reads `/proc/loadavg` on Linux. When the
1-minute load average exceeds `1.5 × num_cpus`, returns `2.0`. The
cadence formula multiplies in.

```rust
#[cfg(target_os = "linux")]
fn loadavg_multiplier() -> f32 {
    let raw = std::fs::read_to_string("/proc/loadavg").ok();
    let load_1m: f32 = raw.split_whitespace().next()?.parse().ok()?;
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f32;
    if load_1m > 1.5 * cpus {
        2.0
    } else {
        1.0
    }
}

#[cfg(not(target_os = "linux"))]
fn loadavg_multiplier() -> f32 { 1.0 }
```

Why 1.5× and not 1.0×: the loop should back off when the system is
genuinely contended, not just when something legitimate is busy.
A 4-core box with load 4.5 is doing real work; load 6.0 is
overloaded.

Non-Linux returns 1.0 (no back-off signal). v1.7 will add Windows
performance-counter equivalent.

## What this enables

- Foreground tool calls always win. The latency of `mind_search`
  on a 12k-base never sees the background loop in the same way it
  never sees a snapshot writer in PostgreSQL.
- The loop runs at a useful cadence under normal load. Default 1
  hour, halves to 5 minutes under high edit rate, doubles to 24h
  under quiet.
- Bursts of edit traffic don't pile up dead candidates. The
  edit counter (`EDITS_SINCE_LAST_TICK`) feeds the cadence —
  busy graph → faster ticks.

## Trade-offs

- **Wall-time scans are slower than they'd be without the busy
  check.** True. Acceptable — the cap means each tick is bounded;
  the busy check is one atomic load per fact (negligible).
- **Non-Linux gets a degraded cadence.** True. Acceptable for v1.5
  / v1.6 (Linux is the only released platform). v1.7 closes the
  gap if Mac / Windows binaries ship (issues #19, #20).
- **The `/proc/loadavg` read is a syscall per multiplier call.**
  Once per tick — cadence is at most twice per minute under heavy
  edit traffic. Not measurable.

## Tests pinning the guarantees

- `doubt::tests::busy_flag_observable_by_loop_check` — guarantee (a)
  plumbing.
- `doubt::tests::per_tick_cap_enforced_by_drain` — guarantee (b) at
  the drain layer.
- `doubt::tests::drain_then_reflag_preserves_registry` — failure
  path keeps work in the queue.
- `doubt::tests::loadavg_multiplier_is_one_when_idle` — non-doubling
  default.
- `doubt::tests::loadavg_multiplier_doubles_above_threshold` —
  formula contract.

The actual spawn is exercised only at the helper level. Spawning
the loop against a stubbed Qdrant proved too flaky for parallel
test runs (race on global MCP_BUSY / DOUBT_WINDOW_FLAGGED
registries). The registry tests are the stable equivalent.

## Alternatives considered

- **Single global lock around all reads + the loop.** Rejected —
  collapses MCP throughput to 1 request at a time.
- **Cooperative yielding via `tokio::task::yield_now`.** Considered;
  doesn't help the loop because the contention is at the Qdrant
  client level, not the tokio scheduler level.
- **Rate-limiting via `governor` crate.** Rejected — adds a
  dependency for a use case the atomic + cap pattern handles
  inline with zero overhead.

## Future revisions

- Mac equivalent of `/proc/loadavg` via `getloadavg(3)`.
- Windows equivalent via `GetSystemTimes` + delta calculation.
- v2.0 — adaptive cap based on prior tick wall-time (slow ticks
  reduce next-tick cap, fast ticks raise it).
