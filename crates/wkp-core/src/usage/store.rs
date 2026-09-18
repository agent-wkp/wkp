//! `.wkp/metrics.db` (design 5.5, ADR-0016): a fixed-slot SQLite ring
//! buffer for tool-invocation counters and gauges. Writes never grow the
//! file once every slot has been touched once — each `(metric, tier)`
//! pair owns exactly `tier.slots` rows, addressed by `slot =
//! bucket_index % tier.slots` and written with `UPDATE`
//! (`INSERT ... ON CONFLICT DO UPDATE`), never `INSERT`/`DELETE`.
//!
//! Deliberately out of scope here (tracked as follow-up, issue #249):
//! wiring these calls into `wkp-cli`'s dispatch, the `wkp usage` CLI
//! command's output formatting, and anything hub-side (design 8.4).

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wkp_sys::rusqlite;
use wkp_sys::rusqlite::OptionalExtension;

use super::schema::{MetricKind, UsageError, TIERS};
use super::Connection;

/// Opens (creating if absent) `.wkp/metrics.db`. WAL + a short busy
/// timeout, unlike `index.db`'s deliberately-default journal mode
/// (`wkp_sys::open`'s own doc comment): unlike the index, this file *is*
/// written concurrently by more than one process (two agent sessions
/// against the same store), and losing the last few samples to a crash
/// is a non-event here, which is why `synchronous = NORMAL` (not
/// `FULL`) is an acceptable choice specifically for this file.
pub fn open_usage_db(path: &Path) -> Result<Connection, UsageError> {
    let conn = wkp_sys::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(Duration::from_millis(500))?;
    create_schema(&conn)?;
    Ok(conn)
}

fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS metrics (
            id   INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            kind TEXT NOT NULL CHECK (kind IN ('counter', 'gauge'))
        );
        CREATE TABLE IF NOT EXISTS raw (
            metric_id    INTEGER NOT NULL,
            slot         INTEGER NOT NULL,
            bucket_start INTEGER NOT NULL,
            n            INTEGER NOT NULL,
            sum          REAL    NOT NULL,
            min          REAL    NOT NULL,
            max          REAL    NOT NULL,
            last         REAL    NOT NULL,
            PRIMARY KEY (metric_id, slot)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS agg_15m (
            metric_id    INTEGER NOT NULL,
            slot         INTEGER NOT NULL,
            bucket_start INTEGER NOT NULL,
            n            INTEGER NOT NULL,
            sum          REAL    NOT NULL,
            min          REAL    NOT NULL,
            max          REAL    NOT NULL,
            last         REAL    NOT NULL,
            PRIMARY KEY (metric_id, slot)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS agg_6h (
            metric_id    INTEGER NOT NULL,
            slot         INTEGER NOT NULL,
            bucket_start INTEGER NOT NULL,
            n            INTEGER NOT NULL,
            sum          REAL    NOT NULL,
            min          REAL    NOT NULL,
            max          REAL    NOT NULL,
            last         REAL    NOT NULL,
            PRIMARY KEY (metric_id, slot)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS cursors (
            metric_id    INTEGER NOT NULL,
            tier_idx     INTEGER NOT NULL,
            slot         INTEGER NOT NULL,
            bucket_start INTEGER NOT NULL,
            PRIMARY KEY (metric_id, tier_idx)
        ) WITHOUT ROWID;",
    )
}

fn get_or_create_metric(
    conn: &Connection,
    name: &str,
    kind: MetricKind,
) -> Result<i64, UsageError> {
    let existing = conn
        .query_row(
            "SELECT id, kind FROM metrics WHERE name = ?1",
            [name],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?;
    if let Some((id, stored_kind)) = existing {
        if stored_kind != kind.as_str() {
            return Err(UsageError::MetricKindMismatch {
                name: name.to_string(),
                requested: kind,
                stored: stored_kind,
            });
        }
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO metrics (name, kind) VALUES (?1, ?2)",
        rusqlite::params![name, kind.as_str()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// One slot's accumulated stats. `n`/`sum` give a counter's call count
/// and total latency (`sum/n` = mean); `last` is a gauge's actually-
/// useful value (design 5.5).
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

    /// Combines `self` with `incoming`. `incoming_is_newer` decides whose
    /// `last` survives — chronological order, not merge order (the
    /// out-of-order-arrival guard in `apply` merges an older sample into
    /// an already-fresher slot without incoming taking `last`).
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

struct StoredSlot {
    agg: SlotAgg,
}

fn floor_to_step(secs: i64, step: i64) -> i64 {
    secs.div_euclid(step) * step
}

fn slot_for(bucket_start: i64, step_secs: i64, slots: i64) -> i64 {
    bucket_start.div_euclid(step_secs).rem_euclid(slots)
}

fn fetch_slot(
    conn: &Connection,
    table: &str,
    metric_id: i64,
    slot: i64,
) -> rusqlite::Result<Option<StoredSlot>> {
    conn.query_row(
        &format!("SELECT n, sum, min, max, last FROM {table} WHERE metric_id = ?1 AND slot = ?2"),
        rusqlite::params![metric_id, slot],
        |r| {
            Ok(StoredSlot {
                agg: SlotAgg {
                    n: r.get(0)?,
                    sum: r.get(1)?,
                    min: r.get(2)?,
                    max: r.get(3)?,
                    last: r.get(4)?,
                },
            })
        },
    )
    .optional()
}

fn write_slot(
    conn: &Connection,
    table: &str,
    metric_id: i64,
    slot: i64,
    bucket_start: i64,
    agg: SlotAgg,
) -> rusqlite::Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO {table} (metric_id, slot, bucket_start, n, sum, min, max, last)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT (metric_id, slot) DO UPDATE SET
                bucket_start = excluded.bucket_start,
                n = excluded.n, sum = excluded.sum,
                min = excluded.min, max = excluded.max, last = excluded.last"
        ),
        rusqlite::params![
            metric_id,
            slot,
            bucket_start,
            agg.n,
            agg.sum,
            agg.min,
            agg.max,
            agg.last
        ],
    )?;
    Ok(())
}

struct Cursor {
    slot: i64,
    bucket_start: i64,
}

/// Which slot currently holds `metric_id`'s *open* window at `tier_idx`,
/// and what bucket that window covers. This is the piece a pure
/// slot-collision check (does the incoming bucket's own `slot =
/// bucket_start % slots` position already hold something) cannot
/// replace: consecutive buckets at a fine step land in *different*
/// slots (raw's minute 0 and minute 1 are slots 0 and 1, not the same
/// slot), so nothing would ever look "closed" until the ring physically
/// wrapped — leaving sparse, irregularly-timed usage unflushed into the
/// coarser tiers for as long as it takes that exact slot to be revisited.
/// The cursor tracks logical window closure independently of physical
/// slot placement, so a window closes (and folds forward) the moment the
/// *next* event for that metric falls outside it, regardless of how
/// infrequently the metric is used. One row per `(metric_id, tier_idx)`
/// that ever received an event — bounded by the metric catalog's own
/// size, not by event volume.
fn fetch_cursor(
    conn: &Connection,
    metric_id: i64,
    tier_idx: usize,
) -> rusqlite::Result<Option<Cursor>> {
    conn.query_row(
        "SELECT slot, bucket_start FROM cursors WHERE metric_id = ?1 AND tier_idx = ?2",
        rusqlite::params![metric_id, tier_idx as i64],
        |r| {
            Ok(Cursor {
                slot: r.get(0)?,
                bucket_start: r.get(1)?,
            })
        },
    )
    .optional()
}

fn set_cursor(
    conn: &Connection,
    metric_id: i64,
    tier_idx: usize,
    slot: i64,
    bucket_start: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO cursors (metric_id, tier_idx, slot, bucket_start)
         VALUES (?1,?2,?3,?4)
         ON CONFLICT (metric_id, tier_idx) DO UPDATE SET
            slot = excluded.slot, bucket_start = excluded.bucket_start",
        rusqlite::params![metric_id, tier_idx as i64, slot, bucket_start],
    )?;
    Ok(())
}

/// Applies `incoming` (a single fresh sample at the raw tier, or a whole
/// closed window folded up from a finer tier) to `tier_idx`'s ring for
/// `metric_id` at `bucket_start`. Design 5.5's "fold-forward on bucket
/// close", driven by the cursor above rather than slot collision: if
/// `metric_id`'s currently-open window at this tier is a strictly older
/// bucket than `bucket_start`, that window's accumulated contents are
/// recursively applied one tier up *before* a fresh window is opened at
/// the new bucket (which may land in a different physical slot than the
/// old one) — never dropped, never double-counted (a window is folded
/// exactly once, at the moment it closes).
fn apply(
    conn: &Connection,
    tier_idx: usize,
    metric_id: i64,
    bucket_start: i64,
    incoming: SlotAgg,
) -> rusqlite::Result<()> {
    let tier = &TIERS[tier_idx];
    match fetch_cursor(conn, metric_id, tier_idx)? {
        Some(cursor) if cursor.bucket_start == bucket_start => {
            let existing = fetch_slot(conn, tier.table, metric_id, cursor.slot)?
                .expect("cursor always points at a real, just-written row");
            let merged = existing.agg.merged_with(&incoming, true);
            write_slot(
                conn,
                tier.table,
                metric_id,
                cursor.slot,
                bucket_start,
                merged,
            )
        }
        Some(cursor) if cursor.bucket_start > bucket_start => {
            // Out-of-order arrival (clock skew, or two concurrent
            // writers racing across a bucket boundary): merge into the
            // cursor's own window rather than moving the cursor
            // backwards or letting the older sample's `last` win.
            // Best-effort — this is instrumentation, not a
            // correctness-bearing subsystem (ADR-0016's consequences).
            let existing = fetch_slot(conn, tier.table, metric_id, cursor.slot)?
                .expect("cursor always points at a real, just-written row");
            let merged = existing.agg.merged_with(&incoming, false);
            write_slot(
                conn,
                tier.table,
                metric_id,
                cursor.slot,
                cursor.bucket_start,
                merged,
            )
        }
        Some(cursor) => {
            // The cursor's window is strictly older than `bucket_start`:
            // it just closed. Fold it one tier up, then open a fresh
            // window here.
            let existing = fetch_slot(conn, tier.table, metric_id, cursor.slot)?
                .expect("cursor always points at a real, just-written row");
            if tier_idx + 1 < TIERS.len() {
                let next = &TIERS[tier_idx + 1];
                let next_bucket_start = floor_to_step(cursor.bucket_start, next.step_secs);
                apply(
                    conn,
                    tier_idx + 1,
                    metric_id,
                    next_bucket_start,
                    existing.agg,
                )?;
            }
            let slot = slot_for(bucket_start, tier.step_secs, tier.slots);
            write_slot(conn, tier.table, metric_id, slot, bucket_start, incoming)?;
            set_cursor(conn, metric_id, tier_idx, slot, bucket_start)
        }
        None => {
            // First-ever event for this metric at this tier.
            let slot = slot_for(bucket_start, tier.step_secs, tier.slots);
            write_slot(conn, tier.table, metric_id, slot, bucket_start, incoming)?;
            set_cursor(conn, metric_id, tier_idx, slot, bucket_start)
        }
    }
}

fn record(
    conn: &mut Connection,
    name: &str,
    kind: MetricKind,
    value: f64,
    at: SystemTime,
) -> Result<(), UsageError> {
    let now_secs = at.duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let bucket_start = floor_to_step(now_secs, TIERS[0].step_secs);
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let metric_id = get_or_create_metric(&tx, name, kind)?;
    apply(&tx, 0, metric_id, bucket_start, SlotAgg::single(value))?;
    tx.commit()?;
    Ok(())
}

/// Records one tool invocation's latency. `duration` is stored in
/// milliseconds (fractional), so `sum/n` reads back as mean latency in
/// the same unit design 4.3's latency-target table already uses.
pub fn record_counter(
    conn: &mut Connection,
    name: &str,
    duration: Duration,
    at: SystemTime,
) -> Result<(), UsageError> {
    record(
        conn,
        name,
        MetricKind::Counter,
        duration.as_secs_f64() * 1000.0,
        at,
    )
}

/// Records one gauge sample (e.g. indexed-item count after `wkp index`).
pub fn record_gauge(
    conn: &mut Connection,
    name: &str,
    value: f64,
    at: SystemTime,
) -> Result<(), UsageError> {
    record(conn, name, MetricKind::Gauge, value, at)
}

/// A window's aggregated stats for one metric. `min`/`max`/`last` are
/// meaningless when `n == 0` (no data in the window — either the metric
/// has never been recorded, or every slot that once held data in this
/// window has aged out, per the `bucket_start >= cutoff` filter below).
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

/// The finest tier index whose full span (`step_secs * slots`) still
/// covers `window_secs`, so a query for "the last hour" doesn't get
/// answered out of the 6-hour tier when the 1-minute tier already covers
/// it. Falls back to the coarsest tier for a window wider than anything
/// retains (a best-effort answer over whatever history remains, not an
/// error).
fn pick_tier_idx(window_secs: i64) -> usize {
    TIERS
        .iter()
        .position(|t| t.step_secs * t.slots >= window_secs)
        .unwrap_or(TIERS.len() - 1)
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

/// Aggregates one metric's live data over the trailing `window`, ending
/// at `now`. "Live" means `bucket_start >= now - window`: a slot whose
/// stored bucket predates the window is excluded regardless of whether
/// its row still physically exists — this is what makes an idle period
/// read back as "no data" rather than a previous lap's leftover value
/// (design 5.5).
///
/// A wide window is answered out of a coarse tier (`pick_tier_idx`), but
/// that tier only ever receives a finer tier's data once that finer
/// tier's own window *closes* — so the most recent slice of activity, for
/// however long the finest-relevant window has been open, would otherwise
/// be invisible to a query that (correctly) reads from the coarse tier
/// for its long retention. Each finer tier below the picked one
/// contributes its own currently-open cursor bucket to cover exactly that
/// gap. This can never double-count against the picked tier's own stored
/// rows: a finer tier's open bucket is by construction more recent than
/// anything that has ever folded out of it, so nothing it holds has
/// reached the picked tier's stored rows yet.
pub fn query_window(
    conn: &Connection,
    name: &str,
    window: Duration,
    now: SystemTime,
) -> Result<MetricSummary, UsageError> {
    let metric_id = match conn
        .query_row("SELECT id FROM metrics WHERE name = ?1", [name], |r| {
            r.get::<_, i64>(0)
        })
        .optional()?
    {
        Some(id) => id,
        None => return Ok(MetricSummary::empty()),
    };

    let now_secs = now.duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let window_secs = window.as_secs() as i64;
    let cutoff = now_secs - window_secs;
    let picked_idx = pick_tier_idx(window_secs);
    let tier = &TIERS[picked_idx];

    let mut acc = WindowAcc::default();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT bucket_start, n, sum, min, max, last FROM {} \
             WHERE metric_id = ?1 AND bucket_start >= ?2",
            tier.table
        ))?;
        let rows = stmt.query_map(rusqlite::params![metric_id, cutoff], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                SlotAgg {
                    n: r.get(1)?,
                    sum: r.get(2)?,
                    min: r.get(3)?,
                    max: r.get(4)?,
                    last: r.get(5)?,
                },
            ))
        })?;
        for row in rows {
            let (bucket_start, agg) = row?;
            acc.absorb(bucket_start, &agg);
        }
    }

    for (finer_idx, finer_tier) in TIERS.iter().enumerate().take(picked_idx) {
        if let Some(cursor) = fetch_cursor(conn, metric_id, finer_idx)? {
            if cursor.bucket_start >= cutoff {
                if let Some(slot) = fetch_slot(conn, finer_tier.table, metric_id, cursor.slot)? {
                    acc.absorb(cursor.bucket_start, &slot.agg);
                }
            }
        }
    }

    Ok(acc.into_summary())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs as u64)
    }

    fn open_test_db() -> Connection {
        let conn = wkp_sys::open_in_memory().expect("open in-memory db");
        create_schema(&conn).expect("create schema");
        conn
    }

    #[test]
    fn same_bucket_accumulates() {
        let mut conn = open_test_db();
        record_counter(&mut conn, "search", Duration::from_millis(10), at(10)).unwrap();
        record_counter(&mut conn, "search", Duration::from_millis(20), at(40)).unwrap();

        let s = query_window(&conn, "search", Duration::from_secs(3600), at(50)).unwrap();
        assert_eq!(s.n, 2);
        assert_eq!(s.sum, 30.0);
        assert_eq!(s.min, 10.0);
        assert_eq!(s.max, 20.0);
        assert_eq!(s.last, 20.0);
    }

    #[test]
    fn closing_a_raw_bucket_folds_it_into_the_15m_tier() {
        let mut conn = open_test_db();
        // Minute 0: one sample.
        record_counter(&mut conn, "search", Duration::from_millis(10), at(5)).unwrap();
        // Minute 1: a second sample closes minute 0's raw bucket, which
        // must land in agg_15m (window wide enough that pick_tier
        // chooses agg_15m, and raw's own 6h span still nominally covers
        // both writes -- so this specifically exercises fold-forward,
        // not just tier selection).
        record_counter(&mut conn, "search", Duration::from_millis(20), at(65)).unwrap();

        let metric_id: i64 = conn
            .query_row("SELECT id FROM metrics WHERE name = 'search'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let fifteen_min_row = fetch_slot(&conn, "agg_15m", metric_id, 0).unwrap();
        let row = fifteen_min_row.expect("minute 0's bucket must have folded into agg_15m");
        assert_eq!(row.agg.n, 1);
        assert_eq!(row.agg.sum, 10.0);
    }

    #[test]
    fn cascades_through_two_tiers_on_successive_long_gaps() {
        let mut conn = open_test_db();
        let day = 24 * 3600;

        // Write 1: opens raw's cursor at t=0.
        record_counter(&mut conn, "index", Duration::from_millis(5), at(0)).unwrap();
        // Write 2, 40 days later: closes raw's cursor (folding write 1
        // into agg_15m, where it becomes agg_15m's *own* first-ever
        // cursor -- nothing to cascade further yet, since agg_15m had
        // nothing open before this).
        record_counter(&mut conn, "index", Duration::from_millis(7), at(40 * day)).unwrap();
        // Write 3, another 40 days later: closes raw's cursor again,
        // folding write 2's raw-tier data into agg_15m -- but agg_15m's
        // *own* cursor is still sitting on write 1's window (bucket 0),
        // which this new fold-target bucket doesn't match, so agg_15m's
        // cursor closes too and cascades write 1's data into agg_6h.
        record_counter(&mut conn, "index", Duration::from_millis(9), at(80 * day)).unwrap();

        let metric_id: i64 = conn
            .query_row("SELECT id FROM metrics WHERE name = 'index'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let six_hour_slot = slot_for(0, TIERS[2].step_secs, TIERS[2].slots);
        let row = fetch_slot(&conn, "agg_6h", metric_id, six_hour_slot)
            .unwrap()
            .expect("write 1's sample must have cascaded all the way to agg_6h");
        assert_eq!(row.agg.n, 1);
        assert_eq!(row.agg.sum, 5.0);
    }

    #[test]
    fn out_of_order_arrival_merges_instead_of_clobbering() {
        let mut conn = open_test_db();
        // A later sample commits first (simulating a concurrent writer
        // or clock skew), landing in minute 1's bucket.
        record_counter(&mut conn, "search", Duration::from_millis(20), at(65)).unwrap();
        // An older sample, still within minute 0, arrives second.
        record_counter(&mut conn, "search", Duration::from_millis(10), at(5)).unwrap();

        let metric_id: i64 = conn
            .query_row("SELECT id FROM metrics WHERE name = 'search'", [], |r| {
                r.get(0)
            })
            .unwrap();
        // Minute 1's slot must have absorbed both samples (n=2), not
        // been overwritten by the older one, and must have kept minute
        // 1's own sample as `last` since it really is the newer value.
        let slot = slot_for(60, TIERS[0].step_secs, TIERS[0].slots);
        let row = fetch_slot(&conn, "raw", metric_id, slot).unwrap().unwrap();
        assert_eq!(row.agg.n, 2);
        assert_eq!(row.agg.sum, 30.0);
        assert_eq!(row.agg.last, 20.0);

        // The cursor itself must not have been dragged backwards by the
        // older, out-of-order sample.
        let cursor = fetch_cursor(&conn, metric_id, 0).unwrap().unwrap();
        assert_eq!(cursor.bucket_start, 60);
        assert_eq!(cursor.slot, slot);
    }

    #[test]
    fn a_stale_bucket_reads_back_as_no_data() {
        let mut conn = open_test_db();
        record_counter(&mut conn, "search", Duration::from_millis(10), at(5)).unwrap();

        // Querying a short recent window long after that sample must see
        // nothing, even though the raw tier's own row for that slot
        // still exists on disk (it just hasn't been overwritten yet).
        let long_after = 3 * 3600; // 3 hours later
        let s = query_window(&conn, "search", Duration::from_secs(300), at(long_after)).unwrap();
        assert_eq!(s.n, 0);
    }

    #[test]
    fn unknown_metric_queries_as_empty_not_an_error() {
        let conn = open_test_db();
        let s = query_window(&conn, "never-recorded", Duration::from_secs(3600), at(0)).unwrap();
        assert_eq!(s.n, 0);
        assert_eq!(s.last_at, None);
    }

    #[test]
    fn recording_a_metric_under_two_kinds_is_rejected() {
        let mut conn = open_test_db();
        record_counter(&mut conn, "search", Duration::from_millis(10), at(5)).unwrap();
        let err = record_gauge(&mut conn, "search", 42.0, at(10)).unwrap_err();
        assert!(
            matches!(err, UsageError::MetricKindMismatch { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn a_wide_window_query_still_sees_data_not_yet_folded_out_of_a_finer_tier() {
        let mut conn = open_test_db();
        // A single, very recent sample: still sitting in raw's own
        // currently-open cursor bucket, not yet folded into agg_15m or
        // agg_6h at all (nothing has closed that bucket yet).
        record_counter(&mut conn, "search", Duration::from_millis(10), at(5)).unwrap();

        // A 20-day window can only be answered out of agg_6h (agg_15m's
        // own 14-day span doesn't cover it) -- but agg_6h has no rows for
        // this metric yet at all. Without folding in raw's still-open
        // cursor, this would wrongly read back as "no data".
        let window = Duration::from_secs(20 * 24 * 3600);
        let s = query_window(&conn, "search", window, at(10)).unwrap();
        assert_eq!(s.n, 1);
        assert_eq!(s.sum, 10.0);
        assert_eq!(s.last, 10.0);
    }

    #[test]
    fn gauge_last_value_reflects_the_most_recent_sample_not_the_average() {
        let mut conn = open_test_db();
        record_gauge(&mut conn, "indexed_items", 100.0, at(5)).unwrap();
        record_gauge(&mut conn, "indexed_items", 140.0, at(10)).unwrap();

        let s = query_window(&conn, "indexed_items", Duration::from_secs(3600), at(20)).unwrap();
        assert_eq!(s.n, 2);
        assert_eq!(s.last, 140.0);
        assert_eq!(s.min, 100.0);
        assert_eq!(s.max, 140.0);
    }

    #[test]
    fn repeated_writes_never_grow_past_one_row_per_slot() {
        let mut conn = open_test_db();
        // One write per raw-tier minute for longer than the raw tier's
        // own span (360 minutes): row count for "raw" must plateau at
        // (metrics x slots), never keep growing.
        for minute in 0..500 {
            record_counter(
                &mut conn,
                "search",
                Duration::from_millis(1),
                at(minute * 60),
            )
            .unwrap();
        }
        let row_count: i64 = conn
            .query_row("SELECT count(*) FROM raw", [], |r| r.get(0))
            .unwrap();
        assert_eq!(row_count, TIERS[0].slots);
    }
}
