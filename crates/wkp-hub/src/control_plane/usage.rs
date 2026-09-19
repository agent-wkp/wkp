//! The hub's own operational usage metrics (design 8.4, ADR-0016):
//! front-door request latency, per-tenant indexer runs, control-plane
//! operation counts. Same fixed-slot ring-buffer mechanism as the
//! client's `wkp_core::usage` module (design 5.5) -- fold-forward on
//! bucket close, a cursor tracking logical window closure independent of
//! physical slot placement -- adapted for Postgres and multi-tenancy:
//! one generic `usage_ring`/`usage_cursors` pair (a `tier_idx` column,
//! not one table per tier) plus `usage_metrics_config`, since tier
//! geometry is a runtime, operator-configurable value here rather than a
//! client-side compile-time constant.
//!
//! Scope, deliberately narrow (8.4's own text): this records only what
//! the hub itself does. A device's local `.wkp/metrics.db` is never
//! transmitted here or anywhere else -- there is no ingestion path for
//! client-reported metrics in this module or anywhere in this crate.
//!
//! Deliberately out of scope in this module (tracked as follow-up):
//! wiring these calls into the front door / index-worker's actual
//! request handling, and the `wkp-hub usage` CLI command.

use postgres::{Client, GenericClient};
use time::OffsetDateTime;

use super::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Counter,
    Gauge,
}

impl MetricKind {
    fn as_str(self) -> &'static str {
        match self {
            MetricKind::Counter => "counter",
            MetricKind::Gauge => "gauge",
        }
    }

    /// Only ever called against `usage_metrics.kind`, which a `CHECK`
    /// constraint already restricts to these two strings -- `None` here
    /// would mean that constraint itself failed, not a normal
    /// input-validation case (same reasoning as the client-side
    /// `wkp_core::usage::MetricKind::parse`'s own doc comment).
    fn parse(s: &str) -> Option<MetricKind> {
        match s {
            "counter" => Some(MetricKind::Counter),
            "gauge" => Some(MetricKind::Gauge),
            _ => None,
        }
    }
}

struct TierConfig {
    step_secs: i64,
    slots: i64,
}

/// A fixed advisory-lock key naming "tier geometry is stable right now"
/// -- readers/writers of ring data (`record`, `query_window`) take this
/// *shared* for the duration of their own transaction/query, and
/// `set_tier_config` takes it *exclusive* around its own read-modify-reset
/// sequence. Postgres blocks an exclusive request against any held shared
/// lock and vice versa, so a `record()` transaction can never observe
/// geometry that changes out from under it mid-transaction, and
/// `set_tier_config` can never reset a tier while a write using the old
/// geometry is still in flight. Same precedent as `schema.rs`'s own
/// `SCHEMA_LOCK_KEY` for its analogous concurrent-creation race, just
/// with the shared/exclusive pair since this one has real readers.
const TIER_CONFIG_LOCK_KEY: i64 = 0x776b705f75736167; // "wkp_usag" as bytes

/// Reads the current tier geometry from `usage_metrics_config`, ordered
/// by `tier_idx`. Generic over `GenericClient` so both a plain `Client`
/// (the read path, `query_window`/`list_metrics`) and a `Transaction`
/// (the write path, `apply`, which must see the same geometry the rest
/// of its own transaction commits against) can call it without two
/// near-identical copies. Callers are responsible for holding
/// `TIER_CONFIG_LOCK_KEY` (shared) for as long as the returned geometry
/// stays in play -- this function itself only reads the table.
fn load_tier_config<C: GenericClient>(client: &mut C) -> Result<Vec<TierConfig>, Error> {
    // `WHERE tier_idx IN (0, 1, 2)`, not an unfiltered `SELECT *`: the
    // three canonical tiers are the only ones `apply`/`query_window`
    // know how to address (their own `tier_idx` parameter is a Rust
    // array *position* into this function's returned `Vec`, matched
    // against the ring/cursor tables' `tier_idx` column by that same
    // position) -- an operator row at, say, tier_idx=99 would otherwise
    // silently become an unreachable 4th tier rather than being rejected
    // up front, which `set_tier_config`'s own validation does instead.
    let rows = client.query(
        "SELECT step_secs, slots FROM usage_metrics_config WHERE tier_idx IN (0, 1, 2) ORDER BY tier_idx",
        &[],
    )?;
    Ok(rows
        .iter()
        .map(|r| TierConfig {
            step_secs: r.get(0),
            slots: r.get(1),
        })
        .collect())
}

/// Changes tier `tier_idx`'s step/slot geometry and, in the same
/// transaction, deletes every existing `usage_ring`/`usage_cursors` row
/// for that tier across every tenant and metric -- a slot index computed
/// under the old geometry is meaningless under the new one, so leaving
/// old rows in place would silently misattribute data to the wrong
/// bucket rather than just losing history the operator already accepted
/// losing by changing the config. This is the "coordinated reset"
/// design 8.4 calls for, enforced structurally rather than left as a
/// documented caveat an operator has to remember by hand.
pub fn set_tier_config(
    client: &mut Client,
    tier_idx: i32,
    step_secs: i64,
    slots: i64,
) -> Result<(), Error> {
    if !(0..=2).contains(&tier_idx) {
        return Err(Error::InvalidInput(format!(
            "tier_idx must be 0, 1, or 2 (the three canonical tiers load_tier_config recognizes), got {tier_idx}"
        )));
    }
    if step_secs < 1 || slots < 1 {
        return Err(Error::InvalidInput(format!(
            "step_secs and slots must both be >= 1 (a zero or negative step/slot count makes \
             floor_to_step/slot_for divide by zero on every later record() call for this tier), \
             got step_secs={step_secs}, slots={slots}"
        )));
    }
    let mut tx = client.transaction()?;
    // Exclusive: blocks until every in-flight record()/query_window
    // holding the shared lock has finished, and blocks any new one from
    // starting until this transaction commits or rolls back -- see
    // TIER_CONFIG_LOCK_KEY's own doc comment.
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&TIER_CONFIG_LOCK_KEY])?;
    tx.execute(
        "INSERT INTO usage_metrics_config (tier_idx, step_secs, slots) VALUES ($1, $2, $3)
         ON CONFLICT (tier_idx) DO UPDATE SET step_secs = excluded.step_secs, slots = excluded.slots",
        &[&tier_idx, &step_secs, &slots],
    )?;
    tx.execute("DELETE FROM usage_ring WHERE tier_idx = $1", &[&tier_idx])?;
    tx.execute(
        "DELETE FROM usage_cursors WHERE tier_idx = $1",
        &[&tier_idx],
    )?;
    tx.commit()?;
    Ok(())
}

fn get_or_create_metric<C: GenericClient>(
    client: &mut C,
    name: &str,
    kind: MetricKind,
) -> Result<i64, Error> {
    // One atomic upsert, not a SELECT followed by a separate INSERT: two
    // concurrent callers racing to register the same brand-new metric
    // name would otherwise both pass the "does it exist" check before
    // either commits, and the loser would hit `usage_metrics_name_key`'s
    // unique-constraint violation instead of a harmless no-op -- the
    // exact race `schema.rs`'s own module doc describes finding for
    // concurrent `CREATE TABLE IF NOT EXISTS` calls. `DO UPDATE SET name
    // = excluded.name` is a no-op write (never changes `kind`) that
    // exists purely so `ON CONFLICT` still has a row to `RETURNING` --
    // the standard idiom for "insert, or fetch the existing row" in one
    // round trip.
    let row = client.query_one(
        "INSERT INTO usage_metrics (name, kind) VALUES ($1, $2)
         ON CONFLICT (name) DO UPDATE SET name = excluded.name
         RETURNING id, kind",
        &[&name, &kind.as_str()],
    )?;
    let id: i64 = row.get(0);
    let stored_kind: String = row.get(1);
    if stored_kind != kind.as_str() {
        return Err(Error::InvalidInput(format!(
            "metric '{name}' is already registered as '{stored_kind}', cannot record it as '{}'",
            kind.as_str()
        )));
    }
    Ok(id)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct SlotAgg {
    n: i64,
    sum: f64,
    min: f64,
    max: f64,
    last: f64,
}

impl SlotAgg {
    fn single(v: f64) -> Self {
        Self {
            n: 1,
            sum: v,
            min: v,
            max: v,
            last: v,
        }
    }

    fn merged_with(&self, incoming: &SlotAgg, incoming_is_newer: bool) -> SlotAgg {
        SlotAgg {
            n: self.n + incoming.n,
            sum: self.sum + incoming.sum,
            min: self.min.min(incoming.min),
            max: self.max.max(incoming.max),
            last: if incoming_is_newer {
                incoming.last
            } else {
                self.last
            },
        }
    }
}

fn floor_to_step(secs: i64, step: i64) -> i64 {
    secs.div_euclid(step) * step
}

fn slot_for(bucket_start: i64, step_secs: i64, slots: i64) -> i64 {
    bucket_start.div_euclid(step_secs).rem_euclid(slots)
}

fn fetch_slot(
    tx: &mut postgres::Transaction<'_>,
    tenant_id: i64,
    metric_id: i64,
    tier_idx: i32,
    slot: i64,
) -> Result<Option<SlotAgg>, Error> {
    let row = tx.query_opt(
        "SELECT n, sum, min, max, last FROM usage_ring \
         WHERE tenant_id = $1 AND metric_id = $2 AND tier_idx = $3 AND slot = $4",
        &[&tenant_id, &metric_id, &tier_idx, &slot],
    )?;
    Ok(row.map(|r| SlotAgg {
        n: r.get(0),
        sum: r.get(1),
        min: r.get(2),
        max: r.get(3),
        last: r.get(4),
    }))
}

fn write_slot(
    tx: &mut postgres::Transaction<'_>,
    tenant_id: i64,
    metric_id: i64,
    tier_idx: i32,
    slot: i64,
    bucket_start: i64,
    agg: SlotAgg,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO usage_ring (tenant_id, metric_id, tier_idx, slot, bucket_start, n, sum, min, max, last)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
         ON CONFLICT (tenant_id, metric_id, tier_idx, slot) DO UPDATE SET
            bucket_start = excluded.bucket_start,
            n = excluded.n, sum = excluded.sum,
            min = excluded.min, max = excluded.max, last = excluded.last",
        &[
            &tenant_id,
            &metric_id,
            &tier_idx,
            &slot,
            &bucket_start,
            &agg.n,
            &agg.sum,
            &agg.min,
            &agg.max,
            &agg.last,
        ],
    )?;
    Ok(())
}

struct Cursor {
    slot: i64,
    bucket_start: i64,
}

fn fetch_cursor(
    tx: &mut postgres::Transaction<'_>,
    tenant_id: i64,
    metric_id: i64,
    tier_idx: i32,
) -> Result<Option<Cursor>, Error> {
    let row = tx.query_opt(
        "SELECT slot, bucket_start FROM usage_cursors \
         WHERE tenant_id = $1 AND metric_id = $2 AND tier_idx = $3",
        &[&tenant_id, &metric_id, &tier_idx],
    )?;
    Ok(row.map(|r| Cursor {
        slot: r.get(0),
        bucket_start: r.get(1),
    }))
}

fn set_cursor(
    tx: &mut postgres::Transaction<'_>,
    tenant_id: i64,
    metric_id: i64,
    tier_idx: i32,
    slot: i64,
    bucket_start: i64,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO usage_cursors (tenant_id, metric_id, tier_idx, slot, bucket_start)
         VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (tenant_id, metric_id, tier_idx) DO UPDATE SET
            slot = excluded.slot, bucket_start = excluded.bucket_start",
        &[&tenant_id, &metric_id, &tier_idx, &slot, &bucket_start],
    )?;
    Ok(())
}

/// Applies `incoming` to `tier_idx`'s ring for `(tenant_id, metric_id)`
/// at `bucket_start`, cascading into coarser tiers as needed. Identical
/// algorithm to the client's own `wkp_core::usage::store::apply` (design
/// 5.5) -- see that function's doc comment for the full reasoning (the
/// cursor-vs-slot-collision distinction, the out-of-order-arrival guard).
/// The one structural difference: `tiers` is a runtime-loaded `Vec`
/// (`usage_metrics_config`, possibly operator-overridden), not a
/// compile-time array, so tier count/geometry come from the caller
/// rather than a `TIERS` constant.
fn apply(
    tx: &mut postgres::Transaction<'_>,
    tiers: &[TierConfig],
    tier_idx: usize,
    tenant_id: i64,
    metric_id: i64,
    bucket_start: i64,
    incoming: SlotAgg,
) -> Result<(), Error> {
    let tier = &tiers[tier_idx];
    match fetch_cursor(tx, tenant_id, metric_id, tier_idx as i32)? {
        Some(cursor) if cursor.bucket_start == bucket_start => {
            let existing = fetch_slot(tx, tenant_id, metric_id, tier_idx as i32, cursor.slot)?
                .expect("cursor always points at a real, just-written row");
            let merged = existing.merged_with(&incoming, true);
            write_slot(
                tx,
                tenant_id,
                metric_id,
                tier_idx as i32,
                cursor.slot,
                bucket_start,
                merged,
            )
        }
        Some(cursor) if cursor.bucket_start > bucket_start => {
            let existing = fetch_slot(tx, tenant_id, metric_id, tier_idx as i32, cursor.slot)?
                .expect("cursor always points at a real, just-written row");
            let merged = existing.merged_with(&incoming, false);
            write_slot(
                tx,
                tenant_id,
                metric_id,
                tier_idx as i32,
                cursor.slot,
                cursor.bucket_start,
                merged,
            )
        }
        Some(cursor) => {
            let existing = fetch_slot(tx, tenant_id, metric_id, tier_idx as i32, cursor.slot)?
                .expect("cursor always points at a real, just-written row");
            if tier_idx + 1 < tiers.len() {
                let next_bucket_start =
                    floor_to_step(cursor.bucket_start, tiers[tier_idx + 1].step_secs);
                apply(
                    tx,
                    tiers,
                    tier_idx + 1,
                    tenant_id,
                    metric_id,
                    next_bucket_start,
                    existing,
                )?;
            }
            let slot = slot_for(bucket_start, tier.step_secs, tier.slots);
            write_slot(
                tx,
                tenant_id,
                metric_id,
                tier_idx as i32,
                slot,
                bucket_start,
                incoming,
            )?;
            set_cursor(
                tx,
                tenant_id,
                metric_id,
                tier_idx as i32,
                slot,
                bucket_start,
            )
        }
        None => {
            let slot = slot_for(bucket_start, tier.step_secs, tier.slots);
            write_slot(
                tx,
                tenant_id,
                metric_id,
                tier_idx as i32,
                slot,
                bucket_start,
                incoming,
            )?;
            set_cursor(
                tx,
                tenant_id,
                metric_id,
                tier_idx as i32,
                slot,
                bucket_start,
            )
        }
    }
}

fn record(
    client: &mut Client,
    tenant_id: i64,
    name: &str,
    kind: MetricKind,
    value: f64,
    at: OffsetDateTime,
) -> Result<(), Error> {
    let mut tx = client.transaction()?;
    let metric_id = get_or_create_metric(&mut tx, name, kind)?;

    // Shared: see TIER_CONFIG_LOCK_KEY's own doc comment -- blocks a
    // concurrent set_tier_config from resetting a tier's geometry while
    // this transaction is still relying on it.
    tx.execute(
        "SELECT pg_advisory_xact_lock_shared($1)",
        &[&TIER_CONFIG_LOCK_KEY],
    )?;

    // Exclusive, scoped to this one (tenant, metric) pair: serializes
    // concurrent record() calls for the same metric so two callers can
    // never both observe "no cursor yet" and independently write a
    // single-sample row, silently discarding one of the two samples --
    // write_slot's own `ON CONFLICT DO UPDATE` replaces a slot's
    // aggregate wholesale, there is no server-side "add to whatever's
    // already there" fallback. Held for the whole transaction, so the
    // recursive `apply` cascade across tiers is covered by this one
    // acquisition, not a separate one per tier. The multiplicative
    // combine is a cheap, deterministic hash, not a guaranteed-unique
    // key -- a rare collision only costs two unrelated metrics some
    // extra serialization, never a correctness problem, since the real
    // row-level uniqueness still comes from the primary keys themselves.
    let metric_lock_key = tenant_id.wrapping_mul(1_000_003).wrapping_add(metric_id);
    tx.execute("SELECT pg_advisory_xact_lock($1)", &[&metric_lock_key])?;

    let tiers = load_tier_config(&mut tx)?;
    if tiers.is_empty() {
        return Err(Error::InvalidInput(
            "usage_metrics_config has no tiers -- schema not initialized correctly".to_string(),
        ));
    }
    let bucket_start = floor_to_step(at.unix_timestamp(), tiers[0].step_secs);
    apply(
        &mut tx,
        &tiers,
        0,
        tenant_id,
        metric_id,
        bucket_start,
        SlotAgg::single(value),
    )?;
    tx.commit()?;
    Ok(())
}

/// Records one operation's latency for `tenant_id` (e.g. one front-door
/// git request, one index-worker run). `duration` in milliseconds, same
/// unit convention as the client side.
pub fn record_counter(
    client: &mut Client,
    tenant_id: i64,
    name: &str,
    duration: std::time::Duration,
    at: OffsetDateTime,
) -> Result<(), Error> {
    record(
        client,
        tenant_id,
        name,
        MetricKind::Counter,
        duration.as_secs_f64() * 1000.0,
        at,
    )
}

/// Records one gauge sample for `tenant_id`.
pub fn record_gauge(
    client: &mut Client,
    tenant_id: i64,
    name: &str,
    value: f64,
    at: OffsetDateTime,
) -> Result<(), Error> {
    record(client, tenant_id, name, MetricKind::Gauge, value, at)
}

/// A window's aggregated stats for one tenant's metric. See
/// `wkp_core::usage::MetricSummary`'s own doc comment: `min`/`max`/`last`
/// are meaningless when `n == 0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MetricSummary {
    pub n: i64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
    pub last: f64,
    pub last_at: Option<i64>,
}

impl MetricSummary {
    fn empty() -> Self {
        Self {
            n: 0,
            sum: 0.0,
            min: 0.0,
            max: 0.0,
            last: 0.0,
            last_at: None,
        }
    }
}

fn pick_tier_idx(tiers: &[TierConfig], window_secs: i64) -> usize {
    tiers
        .iter()
        .position(|t| t.step_secs * t.slots >= window_secs)
        .unwrap_or(tiers.len().saturating_sub(1))
}

#[derive(Default)]
struct WindowAcc {
    n: i64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    last: f64,
    last_at: Option<i64>,
}

impl WindowAcc {
    fn absorb(&mut self, bucket_start: i64, agg: &SlotAgg) {
        self.n += agg.n;
        self.sum += agg.sum;
        self.min = Some(self.min.map_or(agg.min, |m| m.min(agg.min)));
        self.max = Some(self.max.map_or(agg.max, |m| m.max(agg.max)));
        if self.last_at.is_none_or(|la| bucket_start > la) {
            self.last = agg.last;
            self.last_at = Some(bucket_start);
        }
    }

    fn into_summary(self) -> MetricSummary {
        MetricSummary {
            n: self.n,
            sum: self.sum,
            min: self.min.unwrap_or(0.0),
            max: self.max.unwrap_or(0.0),
            last: self.last,
            last_at: self.last_at,
        }
    }
}

/// Aggregates one tenant's metric over the trailing `window`, ending at
/// `now`. Same semantics as `wkp_core::usage::query_window` (design 5.5):
/// a slot whose `bucket_start` predates the window is excluded regardless
/// of whether its row still exists, and a wide window also pulls in any
/// finer tier's still-open cursor bucket, since that data hasn't folded
/// up into the coarser tier yet.
///
/// Unlike `record`, this does not hold `TIER_CONFIG_LOCK_KEY` across its
/// several separate queries -- a `set_tier_config` reset committing
/// midway through can at worst make this read pick a tier boundary that
/// no longer matches a just-reset tier's (now-empty) rows, undercounting
/// for one query during the rare window an operator is actively
/// reconfiguring retention. Self-correcting on the next call, never
/// corrupts anything (only `record`'s writes can do that, which is what
/// the shared lock there actually guards against) -- accepted for a
/// best-effort read path rather than adding transaction-wrapping
/// complexity here too.
pub fn query_window(
    client: &mut Client,
    tenant_id: i64,
    name: &str,
    window: std::time::Duration,
    now: OffsetDateTime,
) -> Result<MetricSummary, Error> {
    let metric_id =
        match client.query_opt("SELECT id FROM usage_metrics WHERE name = $1", &[&name])? {
            Some(row) => row.get::<_, i64>(0),
            None => return Ok(MetricSummary::empty()),
        };

    let tiers = load_tier_config(client)?;
    if tiers.is_empty() {
        return Ok(MetricSummary::empty());
    }

    let now_secs = now.unix_timestamp();
    let window_secs = window.as_secs() as i64;
    let cutoff = now_secs - window_secs;
    let picked_idx = pick_tier_idx(&tiers, window_secs);

    let mut acc = WindowAcc::default();
    let rows = client.query(
        "SELECT bucket_start, n, sum, min, max, last FROM usage_ring \
         WHERE tenant_id = $1 AND metric_id = $2 AND tier_idx = $3 AND bucket_start >= $4",
        &[&tenant_id, &metric_id, &(picked_idx as i32), &cutoff],
    )?;
    for row in &rows {
        let bucket_start: i64 = row.get(0);
        let agg = SlotAgg {
            n: row.get(1),
            sum: row.get(2),
            min: row.get(3),
            max: row.get(4),
            last: row.get(5),
        };
        acc.absorb(bucket_start, &agg);
    }

    for finer_idx in 0..picked_idx {
        if let Some(row) = client.query_opt(
            "SELECT slot, bucket_start FROM usage_cursors \
             WHERE tenant_id = $1 AND metric_id = $2 AND tier_idx = $3",
            &[&tenant_id, &metric_id, &(finer_idx as i32)],
        )? {
            let slot: i64 = row.get(0);
            let bucket_start: i64 = row.get(1);
            if bucket_start >= cutoff {
                if let Some(row) = client.query_opt(
                    "SELECT n, sum, min, max, last FROM usage_ring \
                     WHERE tenant_id = $1 AND metric_id = $2 AND tier_idx = $3 AND slot = $4",
                    &[&tenant_id, &metric_id, &(finer_idx as i32), &slot],
                )? {
                    let agg = SlotAgg {
                        n: row.get(0),
                        sum: row.get(1),
                        min: row.get(2),
                        max: row.get(3),
                        last: row.get(4),
                    };
                    acc.absorb(bucket_start, &agg);
                }
            }
        }
    }

    Ok(acc.into_summary())
}

/// Every metric name and kind the catalog knows about, ordered by name.
pub fn list_metrics(client: &mut Client) -> Result<Vec<(String, MetricKind)>, Error> {
    let rows = client.query("SELECT name, kind FROM usage_metrics ORDER BY name", &[])?;
    let mut out = Vec::new();
    for row in &rows {
        let name: String = row.get(0);
        let kind_str: String = row.get(1);
        let kind = MetricKind::parse(&kind_str).expect(
            "usage_metrics.kind has a CHECK constraint restricting it to 'counter'/'gauge'",
        );
        out.push((name, kind));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test gets its own tenant, same reasoning as
    /// `control_plane::tests::unique_slug`: these tests share one real
    /// Postgres database (the CI/local service container), run
    /// concurrently, and never truncate tables between runs. Isolating
    /// by tenant_id is what keeps ring-buffer state independent across
    /// tests -- `usage_metrics_config` (tier geometry) is the one piece
    /// of state this module introduces that is *not* tenant-scoped, so
    /// no test here calls `set_tier_config` with anything other than an
    /// out-of-range `tier_idx` (a pure validation path, no shared-state
    /// mutation) -- see `set_tier_config_rejects_an_out_of_range_tier_idx`.
    fn unique_tenant(client: &mut Client, prefix: &str) -> super::super::Tenant {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        super::super::create_tenant(client, &format!("usage-test-{prefix}-{nanos}"))
            .expect("create_tenant")
    }

    /// A metric name unique to this test run, same reasoning as
    /// `unique_tenant` -- `usage_metrics.name` is globally unique, and a
    /// fixed literal would collide with a previous run's leftover row.
    fn unique_metric(prefix: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{prefix}-{nanos}")
    }

    fn at(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(secs).expect("valid unix timestamp")
    }

    fn connect() -> Client {
        super::super::connect().expect("connect (is DATABASE_URL set?)")
    }

    #[test]
    fn same_bucket_accumulates() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "same-bucket");
        let metric = unique_metric("search");
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(10),
            at(10),
        )
        .unwrap();
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(20),
            at(40),
        )
        .unwrap();

        let s = query_window(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_secs(3600),
            at(50),
        )
        .unwrap();
        assert_eq!(s.n, 2);
        assert_eq!(s.sum, 30.0);
        assert_eq!(s.min, 10.0);
        assert_eq!(s.max, 20.0);
        assert_eq!(s.last, 20.0);
    }

    #[test]
    fn closing_a_raw_bucket_folds_it_into_tier_1() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "fold");
        let metric = unique_metric("search");
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(10),
            at(5),
        )
        .unwrap();
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(20),
            at(65),
        )
        .unwrap();

        let row = client
            .query_opt(
                "SELECT n, sum FROM usage_ring WHERE tenant_id = $1 AND tier_idx = 1 \
                 AND metric_id = (SELECT id FROM usage_metrics WHERE name = $2)",
                &[&tenant.id, &metric],
            )
            .unwrap()
            .expect("minute 0's bucket must have folded into tier 1");
        let n: i64 = row.get(0);
        let sum: f64 = row.get(1);
        assert_eq!(n, 1);
        assert_eq!(sum, 10.0);
    }

    #[test]
    fn cascades_through_two_tiers_on_successive_long_gaps() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "cascade");
        let metric = unique_metric("index");
        let day = 24 * 3600;

        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(5),
            at(0),
        )
        .unwrap();
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(7),
            at(40 * day),
        )
        .unwrap();
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(9),
            at(80 * day),
        )
        .unwrap();

        let row = client
            .query_opt(
                "SELECT n, sum FROM usage_ring WHERE tenant_id = $1 AND tier_idx = 2 \
                 AND metric_id = (SELECT id FROM usage_metrics WHERE name = $2)",
                &[&tenant.id, &metric],
            )
            .unwrap()
            .expect("write 1's sample must have cascaded all the way to tier 2");
        let n: i64 = row.get(0);
        let sum: f64 = row.get(1);
        assert_eq!(n, 1);
        assert_eq!(sum, 5.0);
    }

    #[test]
    fn out_of_order_arrival_merges_instead_of_clobbering() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "out-of-order");
        let metric = unique_metric("search");
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(20),
            at(65),
        )
        .unwrap();
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(10),
            at(5),
        )
        .unwrap();

        let row = client
            .query_opt(
                "SELECT slot, bucket_start FROM usage_cursors WHERE tenant_id = $1 AND tier_idx = 0 \
                 AND metric_id = (SELECT id FROM usage_metrics WHERE name = $2)",
                &[&tenant.id, &metric],
            )
            .unwrap()
            .unwrap();
        let slot: i64 = row.get(0);
        let bucket_start: i64 = row.get(1);
        assert_eq!(
            bucket_start, 60,
            "cursor must not have been dragged backwards"
        );

        let row = client
            .query_opt(
                "SELECT n, sum, last FROM usage_ring WHERE tenant_id = $1 AND tier_idx = 0 AND slot = $2 \
                 AND metric_id = (SELECT id FROM usage_metrics WHERE name = $3)",
                &[&tenant.id, &slot, &metric],
            )
            .unwrap()
            .unwrap();
        let n: i64 = row.get(0);
        let sum: f64 = row.get(1);
        let last: f64 = row.get(2);
        assert_eq!(n, 2);
        assert_eq!(sum, 30.0);
        assert_eq!(last, 20.0);
    }

    #[test]
    fn a_stale_bucket_reads_back_as_no_data() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "stale");
        let metric = unique_metric("search");
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(10),
            at(5),
        )
        .unwrap();

        let s = query_window(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_secs(300),
            at(3 * 3600),
        )
        .unwrap();
        assert_eq!(s.n, 0);
    }

    #[test]
    fn unknown_metric_queries_as_empty_not_an_error() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "unknown-metric");
        let s = query_window(
            &mut client,
            tenant.id,
            "never-recorded",
            std::time::Duration::from_secs(3600),
            at(0),
        )
        .unwrap();
        assert_eq!(s.n, 0);
        assert_eq!(s.last_at, None);
    }

    #[test]
    fn gauge_last_value_reflects_the_most_recent_sample() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "gauge");
        let metric = unique_metric("indexed_items");
        record_gauge(&mut client, tenant.id, &metric, 100.0, at(5)).unwrap();
        record_gauge(&mut client, tenant.id, &metric, 140.0, at(10)).unwrap();

        let s = query_window(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_secs(3600),
            at(20),
        )
        .unwrap();
        assert_eq!(s.n, 2);
        assert_eq!(s.last, 140.0);
        assert_eq!(s.min, 100.0);
        assert_eq!(s.max, 140.0);
    }

    #[test]
    fn a_wide_window_query_still_sees_data_not_yet_folded_out_of_a_finer_tier() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "wide-window");
        let metric = unique_metric("search");
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(10),
            at(5),
        )
        .unwrap();

        let window = std::time::Duration::from_secs(20 * 24 * 3600);
        let s = query_window(&mut client, tenant.id, &metric, window, at(10)).unwrap();
        assert_eq!(s.n, 1);
        assert_eq!(s.sum, 10.0);
    }

    #[test]
    fn recording_a_metric_under_two_kinds_is_rejected() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "kind-mismatch");
        let metric = unique_metric("search");
        record_counter(
            &mut client,
            tenant.id,
            &metric,
            std::time::Duration::from_millis(10),
            at(5),
        )
        .unwrap();
        let err = record_gauge(&mut client, tenant.id, &metric, 42.0, at(10)).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    }

    #[test]
    fn list_metrics_includes_every_recorded_name_and_kind() {
        let mut client = connect();
        let tenant = unique_tenant(&mut client, "list");
        let counter_name = unique_metric("search");
        let gauge_name = unique_metric("indexed_items");
        record_counter(
            &mut client,
            tenant.id,
            &counter_name,
            std::time::Duration::from_millis(1),
            at(0),
        )
        .unwrap();
        record_gauge(&mut client, tenant.id, &gauge_name, 1.0, at(0)).unwrap();

        let all = list_metrics(&mut client).unwrap();
        assert!(all.contains(&(counter_name, MetricKind::Counter)));
        assert!(all.contains(&(gauge_name, MetricKind::Gauge)));
    }

    #[test]
    fn set_tier_config_rejects_an_out_of_range_tier_idx() {
        // Deliberately never exercises set_tier_config with tier_idx in
        // 0..=2 here: usage_metrics_config is global, not tenant-scoped
        // (module doc comment), so changing real tier geometry would
        // race with every other test in this file running concurrently
        // against the same shared database. The out-of-range rejection
        // path is pure validation -- no DB write happens before the
        // error returns -- so it's the one part of this function safe
        // to exercise here.
        let mut client = connect();
        let err = set_tier_config(&mut client, 3, 60, 100).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
        let err = set_tier_config(&mut client, -1, 60, 100).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    }

    #[test]
    fn set_tier_config_rejects_a_zero_or_negative_step_or_slot_count() {
        // Same reasoning as the out-of-range-tier_idx test above for why
        // this only exercises the validation path (rejected before any
        // DB write) and never a real, in-range reconfiguration.
        let mut client = connect();
        let err = set_tier_config(&mut client, 0, 0, 100).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
        let err = set_tier_config(&mut client, 0, 60, 0).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
        let err = set_tier_config(&mut client, 0, -5, 100).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)), "{err:?}");
    }
}
