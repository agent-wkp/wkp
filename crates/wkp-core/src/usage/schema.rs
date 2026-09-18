//! Fixed shapes for `.wkp/metrics.db` (design 5.5, ADR-0016): the tier
//! table, the metric-kind enum, and the error type. No DDL lives here —
//! `create_schema` (store.rs) owns the actual `CREATE TABLE` statements,
//! since the tier list below and the DDL must never drift apart and
//! keeping them in the same file (store.rs) is how that's enforced by
//! construction, not by comment.

use std::fmt;

use wkp_sys::rusqlite;

/// One resolution tier's step size and slot count. `step_secs * slots`
/// is that tier's total span; each `step_secs` must evenly divide the
/// next tier's `step_secs` so a closed bucket's `bucket_start` maps onto
/// a real boundary one tier up (see `store::apply`'s fold-forward).
pub(super) struct Tier {
    pub(super) table: &'static str,
    pub(super) step_secs: i64,
    pub(super) slots: i64,
}

/// Design 5.5's three tiers: a session's worth of per-minute detail, a
/// couple of weeks of trend, a year of coarse history. Sized for a
/// bursty, low-frequency CLI tool, not a continuously-ticking
/// infrastructure counter — deliberately smaller than RRDtool-typical
/// defaults (see ADR-0016's "client tier sizing" option 7).
pub(super) const TIERS: [Tier; 3] = [
    Tier {
        table: "raw",
        step_secs: 60,
        slots: 360,
    }, // 1 min steps, 6 hours
    Tier {
        table: "agg_15m",
        step_secs: 900,
        slots: 1344,
    }, // 15 min steps, 14 days
    Tier {
        table: "agg_6h",
        step_secs: 21_600,
        slots: 1460,
    }, // 6 hour steps, 1 year
];

/// A counter's slot holds a call count and latency stats (`sum/n` is
/// mean latency); a gauge's slot holds however many samples landed in
/// the window, with `last` the value actually worth reading back (design
/// 5.5: a gauge's current value matters more than its windowed average).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    Counter,
    Gauge,
}

impl MetricKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            MetricKind::Counter => "counter",
            MetricKind::Gauge => "gauge",
        }
    }
}

#[derive(Debug)]
pub enum UsageError {
    Sqlite(rusqlite::Error),
    Time(std::time::SystemTimeError),
    /// A caller tried to record a sample for `name` as `requested`, but
    /// the metrics catalog already has that name registered as
    /// `stored` — e.g. `record_counter` and `record_gauge` called on the
    /// same metric name. A metric's kind decides which column (`sum/n`
    /// vs. `last`) a reader treats as meaningful, so silently accepting
    /// either kind under one name would make `wkp usage`'s output
    /// meaningless for that metric without ever raising an error.
    MetricKindMismatch {
        name: String,
        requested: MetricKind,
        stored: String,
    },
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsageError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            UsageError::Time(e) => write!(f, "system time error: {e}"),
            UsageError::MetricKindMismatch {
                name,
                requested,
                stored,
            } => write!(
                f,
                "metric '{name}' is already registered as '{stored}', cannot record it as '{}'",
                requested.as_str()
            ),
        }
    }
}

impl std::error::Error for UsageError {}

impl From<rusqlite::Error> for UsageError {
    fn from(e: rusqlite::Error) -> Self {
        UsageError::Sqlite(e)
    }
}

impl From<std::time::SystemTimeError> for UsageError {
    fn from(e: std::time::SystemTimeError) -> Self {
        UsageError::Time(e)
    }
}
