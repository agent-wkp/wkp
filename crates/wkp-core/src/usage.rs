//! `.wkp/metrics.db` (design 5.5, ADR-0016): local usage metrics as a
//! fixed-slot SQLite ring buffer — tool-invocation counters (call count,
//! latency) and gauges (e.g. indexed-item count), on by default, never
//! synced and never transmitted anywhere. Bounded size by construction:
//! each `(metric, tier)` pair owns a fixed number of rows, overwritten in
//! place, never grown.
//!
//! Split into modules by concern, same convention as `index`
//! (schema/types vs. the actual read/write logic), re-exported flat from
//! here.
//!
//! Deliberately out of scope in this module (tracked as follow-up, issue
//! #249): wiring these calls into `wkp-cli`'s dispatch, the `wkp usage`
//! CLI command, and anything hub-side (design 8.4).

pub use wkp_sys::rusqlite::Connection;

mod schema;
mod store;

pub use schema::{MetricKind, UsageError};
pub use store::{open_usage_db, query_window, record_counter, record_gauge, MetricSummary};
