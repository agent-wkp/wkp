# ADR-0016: Local usage metrics as a bounded-size SQLite ring buffer; hub metrics scoped to the hub's own operations

Status: accepted
Date: 2026-09-18
Design sections affected: 3.2 (D11 added), 4.3, 5.5 (new), 8.4 (new), 12

## Context

There is no visibility today into how `wkp` actually gets used once an agent has it on `PATH`: which subcommands get invoked, how often, how long each takes, or how the indexed corpus grows over a session or over months. William asked for instrumentation to answer exactly that, with a specific storage shape in mind: fixed database size regardless of how much history accumulates, RRDtool-style, plus a CLI to read it back, plus an equivalent (but explicitly lower-priority) capability at the hub.

Two constraints from `CLAUDE.md` and the existing design shape this more than the feature itself does:

- **D2 (4.2): no mandatory daemon.** `wkp` is invoked as a fresh, short-lived process per subcommand (`crates/wkp-cli/src/main.rs`'s dispatch `match`); there is no always-running process to hold counters in memory or run a periodic rollup/prune job, except `wkpd`, which is opt-in and scoped to sync, not a place to hang metrics infrastructure on.
- **Slim core, `#![forbid(unsafe_code)]` outside `wkp-sys`, "no new on-disk formats."** Any storage choice has to clear the same bar D4a/D4b already set for `index.db`.

## Options

**Storage mechanism (client-side):**

1. **RRDtool via `librrd` FFI bindings.** Gets the exact RRA/consolidation-function model this ADR's storage design borrows conceptually from, for free. Rejected: `librrd` is a C library, so any binding needs `unsafe` FFI outside `wkp-sys` (hard rule violation), and RRDtool's own `.rrd` file is a second on-disk format alongside git-and-markdown and SQLite (also a hard rule violation, "no new on-disk formats").
2. **A pure-Rust or SQLite-wrapping "RRD in a crate" dependency**, if a suitably mature one exists. Rejected on slim-core grounds: the ring-buffer logic this needs (fixed-slot table, `UPDATE`-in-place, a fold-forward rollup on bucket close) is a few hundred lines directly against `rusqlite`, which `wkp-sys` already bundles — a new dependency buys nothing `wkp` doesn't already have the tool to build itself, and every new crate needs `cargo deny`/`cargo vet` justification this wouldn't clear.
3. **A conventional, unbounded event-log table** (`events(ts, tool, duration_ms, ok)`) with periodic `DELETE ... WHERE ts < ?` plus `VACUUM` to reclaim space. Rejected: something has to run the prune job, and per D2 there is no daemon to run it from. Running it inline, opportunistically, on some fraction of invocations risks an occasional multi-second `VACUUM` stall landing on the exact latency-sensitive hot path CLAUDE.md's priority-1 rule protects — a metrics subsystem regressing `wkp search`'s p95 would be a real regression for a feature that exists purely for observability.
4. **A fixed-slot ring buffer in SQLite** (chosen): each `(metric, tier)` pair gets exactly `slot_count` rows, written with `UPDATE` addressed by `slot = bucket_index % slot_count`, never `INSERT`/`DELETE`. Bounded by construction once every slot has been touched once; no `VACUUM`, no daemon, no second file format — `.wkp/metrics.db` is SQLite, exactly like `index.db`.

**Hub scope:**

5. **Hub-ops-only** (chosen): `wkp-hub usage` reports what the hub itself does (front-door latency, per-tenant indexer runs, control-plane operation counts) — data the hub already produces by running tenant pods, requiring no new data ever to leave a device.
6. **Also ingest opt-in client-reported metrics**, aggregated cross-device at the hub. Rejected for this ADR: it would be the second network call `wkp` ever makes, after the already-narrow, explicitly human-configured `--embed-url` exception (5.3) — a bar `AGENTS.md` and `CLAUDE.md`'s "things that look like shortcuts" list both treat as close to sacred (no network call in any default path). No consent flow or wire format exists for this, and nothing in the current milestone scope needs cross-device aggregation enough to justify designing one now. Revisit only against a real, named product need.

**Client tier sizing:**

7. **RRDtool-typical defaults** (e.g. a year of hourly-resolution data on top of a day of minute-resolution data) were the starting point in discussion. Rejected as sized for the wrong access pattern: those defaults assume a continuously-ticking metric (network interface counters, load average), where `wkp` invocations are bursty and sparse (a human or agent runs commands in short clusters, not continuously). At roughly 20 metrics, RRDtool-typical tiers would land `.wkp/metrics.db` around 20-30 MB steady-state for a dataset nobody would query at that resolution.
8. **Three tiers sized around actual CLI usage** (chosen): 1-minute steps for 6 hours (a session), 15-minute steps for 14 days (recent trend), 6-hour steps for 1 year (long-term trend only) — roughly 3,044 slots/metric, ~3.5-4 MB steady-state.

**CLI naming:**

9. **`wkp metrics` / `wkp-hub metrics`.** Direct, RRDtool-flavored, but reads more like an internal subsystem name than a user-facing report.
10. **`wkp usage` / `wkp-hub usage`** (chosen, per William's direction): matches the `/usage`-style convention already established across the agentic-harness CLIs `wkp` is designed to be neutral toward (D2/harness-neutrality priority) — Claude Code, Codex, and similar tools all expose a `usage`/`status`-shaped command for exactly this "how has this been used" question, and `usage` specifically (over `status`, which reads more like a live session/health snapshot) matches a consumption report rather than current state.

## Decision

**Local usage metrics ship as a fixed-slot SQLite ring buffer, `.wkp/metrics.db`, on by default, read via `wkp usage`; hub-side metrics (`wkp-hub usage`) cover only the hub's own server-side operations, never data a device transmits.** Options 4, 5, 8, and 10 above. Full schema, the fold-forward write algorithm, and the concrete tier table are recorded in design section 5.5 (client) and 8.4 (hub) rather than repeated here — this ADR is the decision record, the design doc is the maintained reference.

This is a decision in principle, not a finished implementation. The following remain open, left for the implementing PR(s) rather than decided here, matching this project's existing practice (see ADR-0011's own "genuinely new work" list):

1. **The exact metric catalog and stable integer IDs** for each tracked tool/gauge — the full subcommand list in `crates/wkp-cli/src/main.rs`'s `USAGE` constant plus `indexed_items` and `store_bytes` gauges is the starting set 5.5 assumes, but is not frozen here.
2. **`crates/wkp-cli/src/init.rs`'s gitignore template** needs `.wkp/metrics.db` and its `-wal`/`-shm` siblings added, alongside the existing `.wkp/index.db` entry.
3. **Where the instrumentation wrapper lives in `main.rs`** — one wrapper around the dispatch `match`, per 5.5, but the exact function signature and how exit status is threaded through `std::process::exit` call sites throughout that `match` is implementation detail.
4. **`wkp usage`'s exact output format/columns**, and whether `--json`'s shape is considered a stable contract from day one or allowed to change before a first tagged release.
5. **The hub-side Postgres schema** (`metrics_config` table shape, the `(tenant_id, metric, tier, slot)` table DDL) and `wkp-hub usage`'s CLI wiring, following the existing `tenant create`/`device register` namespacing convention in `crates/wkp-hub/src/main.rs`.
6. **A `benches/` case for the <1ms/3ms per-invocation write budget** (4.3's new row), so this stays measured rather than an assumed engineering target, consistent with how 4.3 already treats its other rows.

## Consequences

- **One new file, no new format.** `.wkp/metrics.db` is SQLite, same as `index.db` — CLAUDE.md's "no new on-disk formats" rule holds. No new dependency, no new `unsafe` code anywhere outside `wkp-sys`'s existing bundled SQLite.
- **A new per-invocation disk write**, now an explicit line in design 4.3's latency-target table (<1ms/3ms, best-effort) so a future regression here is caught the same way every other latency-sensitive path already is, rather than being an untracked side effect.
- **The "no network call outside `--embed-url`" property is unchanged.** Rejecting option 6 above means this ADR adds zero new network surface; a device's usage data never leaves it.
- **CODEOWNERS scope.** `docs/design/` (this ADR's own design-doc edits) and any `crates/wkp-hub/` implementation are already CODEOWNERS-gated paths (`.github/CODEOWNERS`, section 9.1) and need William's review regardless of this ADR. Client-side implementation in `crates/wkp-cli/`/`crates/wkp-core/` is not CODEOWNERS-gated by that file today — normal PR review applies, same as any other CLI subcommand.
- **Distinct from the still-open security-audit gap (8.3's Observability row).** D11's hub metrics are aggregate operational counters, not an identity-attributed "who pushed what ref when" audit trail; that gap remains open, unaddressed by this ADR.
- **Retunable without a migration.** The client's tier constants and the hub's `metrics_config` defaults are both plain numbers, not a schema shape — changing retention later is a constants change (client) or a config-table update (hub), not a data migration.
