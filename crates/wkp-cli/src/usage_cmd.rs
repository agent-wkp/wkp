//! `wkp usage`: reads `.wkp/metrics.db` back (design 5.5, ADR-0016) --
//! per-tool invocation counts, latency, and error counts, or a gauge's
//! current value, over a trailing window. The write side lives entirely
//! in `wkp_core::usage` and `main.rs`'s `record_invocation`; this file is
//! the read-only display layer only.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use wkp_core::usage::MetricKind;

pub(crate) struct UsageOptions {
    pub(crate) path: PathBuf,
    pub(crate) window: Duration,
    pub(crate) tool: Option<String>,
    pub(crate) json: bool,
}

/// Parses `wkp usage [--window 1h|1d|7d|30d] [--tool NAME] [--path DIR]
/// [--json]`. Hand-rolled, same as every other subcommand's flag parsing
/// in this crate (`search::parse_search_args`'s doc comment gives the
/// slim-core reasoning for not reaching for `clap` yet).
pub(crate) fn parse_usage_args(
    mut args: impl Iterator<Item = String>,
) -> Result<UsageOptions, String> {
    let mut path = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut window = Duration::from_secs(24 * 3600);
    let mut tool = None;
    let mut json = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--path" => path = PathBuf::from(args.next().ok_or("--path requires a value")?),
            "--window" => {
                let v = args.next().ok_or("--window requires a value")?;
                window = parse_window(&v)?;
            }
            "--tool" => tool = Some(args.next().ok_or("--tool requires a value")?),
            "--json" => json = true,
            other => return Err(format!("unrecognized argument: {other}")),
        }
    }

    Ok(UsageOptions {
        path,
        window,
        tool,
        json,
    })
}

fn parse_window(s: &str) -> Result<Duration, String> {
    match s {
        "1h" => Ok(Duration::from_secs(3600)),
        "1d" => Ok(Duration::from_secs(24 * 3600)),
        "7d" => Ok(Duration::from_secs(7 * 24 * 3600)),
        "30d" => Ok(Duration::from_secs(30 * 24 * 3600)),
        other => Err(format!(
            "invalid --window value: {other} (expected 1h|1d|7d|30d)"
        )),
    }
}

/// One tool's row in `wkp usage`'s output. `calls`/`errors` only mean
/// something for a counter (a gauge's `errors` is always `0` -- gauges
/// have no `.err` counterpart, per `main.rs::error_metric_name`'s own
/// convention). `avg_ms`/`min_ms`/`max_ms`/`last` are all in whatever
/// unit the metric was recorded in -- milliseconds for every built-in
/// tool-invocation counter, but a gauge's own unit for a gauge (e.g.
/// indexed-item count has no "ms" meaning at all).
struct UsageRow {
    name: String,
    kind: MetricKind,
    calls: i64,
    errors: i64,
    avg: f64,
    min: f64,
    max: f64,
    last: f64,
}

/// Runs `wkp usage`: lists every metric the catalog knows about (minus
/// each counter's own `.err` shadow entry, which folds into its parent
/// row's `errors` column instead of appearing as a row of its own),
/// optionally filtered to one `--tool`, aggregated over `opts.window`.
pub(crate) fn run_usage(opts: &UsageOptions) -> Result<String, String> {
    let metrics_path = opts.path.join(".wkp/metrics.db");
    if !metrics_path.exists() {
        return Ok(
            "wkp: no usage data yet (.wkp/metrics.db doesn't exist -- nothing has been recorded \
             here, or usage metrics haven't run against this store yet)"
                .to_string(),
        );
    }
    let conn =
        wkp_core::usage::open_usage_db_read_only(&metrics_path).map_err(|e| e.to_string())?;
    let now = SystemTime::now();

    let all = wkp_core::usage::list_metrics(&conn).map_err(|e| e.to_string())?;
    let mut rows = Vec::new();
    for (name, kind) in &all {
        if name.ends_with(".err") {
            continue;
        }
        if let Some(only) = &opts.tool {
            if name != only {
                continue;
            }
        }
        let summary = wkp_core::usage::query_window(&conn, name, opts.window, now)
            .map_err(|e| e.to_string())?;
        if summary.n == 0 {
            // No activity in this window -- `min`/`max`/`last` would all
            // read as a meaningless 0.0 (design 5.5's own `MetricSummary`
            // doc comment), indistinguishable from a real reading of
            // zero. Omit the row entirely rather than print it.
            continue;
        }
        let errors = if *kind == MetricKind::Counter {
            wkp_core::usage::query_window(&conn, &format!("{name}.err"), opts.window, now)
                .map_err(|e| e.to_string())?
                .n
        } else {
            0
        };
        rows.push(UsageRow {
            name: name.clone(),
            kind: *kind,
            calls: summary.n,
            errors,
            avg: if summary.n > 0 {
                summary.sum / summary.n as f64
            } else {
                0.0
            },
            min: summary.min,
            max: summary.max,
            last: summary.last,
        });
    }

    Ok(if opts.json {
        format_json(&rows)
    } else {
        format_text(&rows)
    })
}

fn format_text(rows: &[UsageRow]) -> String {
    if rows.is_empty() {
        return "wkp: no usage data in this window".to_string();
    }
    rows.iter()
        .map(|r| match r.kind {
            MetricKind::Counter => format!(
                "{:<20} {:>6} calls  {:>4} errors  avg {:>7.2}ms  min {:>7.2}ms  max {:>7.2}ms",
                r.name, r.calls, r.errors, r.avg, r.min, r.max
            ),
            MetricKind::Gauge => format!(
                "{:<20} {:>6} samples  last={:.2}  min={:.2}  max={:.2}",
                r.name, r.calls, r.last, r.min, r.max
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Hand-rolled, same convention as `search::format_json` (and reusing its
/// `json_string` escaper) -- five/six scalar fields don't justify a new
/// `serde_json` dependency.
fn format_json(rows: &[UsageRow]) -> String {
    let items: Vec<String> = rows
        .iter()
        .map(|r| {
            let kind = match r.kind {
                MetricKind::Counter => "counter",
                MetricKind::Gauge => "gauge",
            };
            format!(
                r#"{{"name":{},"kind":"{}","calls":{},"errors":{},"avg":{},"min":{},"max":{},"last":{}}}"#,
                crate::search::json_string(&r.name),
                kind,
                r.calls,
                r.errors,
                r.avg,
                r.min,
                r.max,
                r.last
            )
        })
        .collect();
    format!("[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_dir(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("wkp-cli-usage-test-{name}-"))
            .tempdir()
            .expect("create temp dir")
    }

    #[test]
    fn parse_usage_args_defaults_to_cwd_1d_window_and_text_format() {
        let opts = parse_usage_args(std::iter::empty()).expect("parse");
        assert_eq!(opts.window, Duration::from_secs(24 * 3600));
        assert_eq!(opts.tool, None);
        assert!(!opts.json);
    }

    #[test]
    fn parse_usage_args_reads_all_flags() {
        let args = [
            "--window",
            "7d",
            "--tool",
            "search",
            "--path",
            "/tmp/store",
            "--json",
        ]
        .into_iter()
        .map(String::from);
        let opts = parse_usage_args(args).expect("parse");
        assert_eq!(opts.window, Duration::from_secs(7 * 24 * 3600));
        assert_eq!(opts.tool.as_deref(), Some("search"));
        assert_eq!(opts.path, PathBuf::from("/tmp/store"));
        assert!(opts.json);
    }

    #[test]
    fn parse_usage_args_rejects_an_invalid_window() {
        let args = ["--window", "3weeks"].into_iter().map(String::from);
        assert!(parse_usage_args(args).is_err());
    }

    #[test]
    fn run_usage_reports_no_data_when_metrics_db_does_not_exist() {
        let dir = store_dir("no-db");
        let opts = UsageOptions {
            path: dir.path().to_path_buf(),
            window: Duration::from_secs(3600),
            tool: None,
            json: false,
        };
        let out = run_usage(&opts).expect("run_usage");
        assert!(out.contains("no usage data yet"), "{out}");
    }

    #[test]
    fn run_usage_reports_calls_errors_and_latency_for_a_counter() {
        let dir = store_dir("counter");
        let db_path = dir.path().join(".wkp/metrics.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let mut conn = wkp_core::usage::open_usage_db(&db_path).expect("open usage db");
        let now = SystemTime::now();
        wkp_core::usage::record_counter(&mut conn, "search", Duration::from_millis(10), now)
            .unwrap();
        wkp_core::usage::record_counter(&mut conn, "search", Duration::from_millis(20), now)
            .unwrap();
        wkp_core::usage::record_counter(&mut conn, "search.err", Duration::from_millis(20), now)
            .unwrap();
        drop(conn);

        let opts = UsageOptions {
            path: dir.path().to_path_buf(),
            window: Duration::from_secs(3600),
            tool: None,
            json: false,
        };
        let out = run_usage(&opts).expect("run_usage");
        assert!(out.contains("search"), "{out}");
        assert!(out.contains("2 calls"), "{out}");
        assert!(out.contains("1 errors"), "{out}");
        // The `.err` shadow metric must never appear as its own row.
        assert!(!out.contains("search.err "), "{out}");
    }

    #[test]
    fn run_usage_json_reports_a_gauges_last_value() {
        let dir = store_dir("gauge");
        let db_path = dir.path().join(".wkp/metrics.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let mut conn = wkp_core::usage::open_usage_db(&db_path).expect("open usage db");
        let now = SystemTime::now();
        wkp_core::usage::record_gauge(&mut conn, "indexed_items", 100.0, now).unwrap();
        wkp_core::usage::record_gauge(&mut conn, "indexed_items", 142.0, now).unwrap();
        drop(conn);

        let opts = UsageOptions {
            path: dir.path().to_path_buf(),
            window: Duration::from_secs(3600),
            tool: None,
            json: true,
        };
        let out = run_usage(&opts).expect("run_usage");
        assert!(out.contains(r#""name":"indexed_items""#), "{out}");
        assert!(out.contains(r#""kind":"gauge""#), "{out}");
        assert!(out.contains(r#""last":142"#), "{out}");
    }

    #[test]
    fn run_usage_tool_filter_excludes_other_metrics() {
        let dir = store_dir("filter");
        let db_path = dir.path().join(".wkp/metrics.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let mut conn = wkp_core::usage::open_usage_db(&db_path).expect("open usage db");
        let now = SystemTime::now();
        wkp_core::usage::record_counter(&mut conn, "search", Duration::from_millis(1), now)
            .unwrap();
        wkp_core::usage::record_counter(&mut conn, "index", Duration::from_millis(1), now).unwrap();
        drop(conn);

        let opts = UsageOptions {
            path: dir.path().to_path_buf(),
            window: Duration::from_secs(3600),
            tool: Some("search".to_string()),
            json: false,
        };
        let out = run_usage(&opts).expect("run_usage");
        assert!(out.contains("search"), "{out}");
        assert!(!out.contains("index "), "{out}");
    }

    #[test]
    fn run_usage_omits_metrics_with_no_activity_in_the_window() {
        let dir = store_dir("stale");
        let db_path = dir.path().join(".wkp/metrics.db");
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let mut conn = wkp_core::usage::open_usage_db(&db_path).expect("open usage db");
        // Recorded well outside the window this test queries -- must not
        // show up as a misleading all-zero row (min/max/last would read
        // as 0.0, indistinguishable from a real zero reading).
        let long_ago = SystemTime::now() - Duration::from_secs(365 * 24 * 3600);
        wkp_core::usage::record_counter(&mut conn, "search", Duration::from_millis(5), long_ago)
            .unwrap();
        drop(conn);

        let opts = UsageOptions {
            path: dir.path().to_path_buf(),
            window: Duration::from_secs(3600),
            tool: None,
            json: false,
        };
        let out = run_usage(&opts).expect("run_usage");
        assert_eq!(out, "wkp: no usage data in this window");
    }
}
