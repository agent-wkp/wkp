//! Regression test for a real bug found running the published v1.0.1
//! release binary by hand (William, 2026-09-16): bare `wkp` (and
//! `wkp --help`/`-h`/`help`, and any unrecognized subcommand) fell
//! through to an M0-era placeholder ("wkp: no subcommands implemented
//! yet") that predated every one of the ~15 real subcommands `main.rs`
//! actually dispatches today -- the first thing a brand-new Homebrew
//! install's user sees when just running `wkp` to check it installed.
//!
//! Drives the real compiled binary as a subprocess (`CARGO_BIN_EXE_wkp`)
//! rather than calling dispatch logic in-process: `main`'s `match` reads
//! real `std::env::args()` and calls `std::process::exit`, neither of
//! which a unit test can safely exercise in-process (see CLAUDE.md's
//! self-invocation section on why process-level behavior needs a real
//! subprocess, not an in-process shortcut).

use std::path::Path;
use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wkp"))
        .args(args)
        .output()
        .expect("run wkp")
}

fn run_in(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wkp"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run wkp")
}

#[test]
fn bare_invocation_prints_real_usage_not_the_stale_placeholder() {
    let output = run(&[]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("no subcommands implemented"),
        "stale placeholder text still present: {stderr}"
    );
    assert!(stderr.contains("Usage: wkp <COMMAND>"), "got: {stderr}");
    // Spot-check a few real subcommands are actually listed.
    for name in ["init", "search", "remember", "promote", "hub register"] {
        assert!(
            stderr.contains(name),
            "usage text missing '{name}': {stderr}"
        );
    }
}

#[test]
fn help_flags_print_usage_and_exit_zero() {
    for flag in ["--help", "-h", "help"] {
        let output = run(&[flag]);
        assert_eq!(output.status.code(), Some(0), "wkp {flag}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("Usage: wkp <COMMAND>"),
            "wkp {flag}: {stdout}"
        );
    }
}

/// Regression test for a real bug William found by hand: `wkp init
/// --help` wasn't recognized -- `init` (unlike every other subcommand)
/// had no argument parser at all, so `args.next()` blindly took
/// `--help` as the literal path argument and created a bogus nested
/// store at `./--help/` instead of printing help. `import` shared the
/// identical gap (same inline `args.next().map(PathBuf::from)` pattern,
/// no parser function). Both now go through a real guard before
/// `init`/`import`'s own `run_*` ever sees a path.
#[test]
fn init_help_prints_usage_and_creates_no_bogus_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_in(dir.path(), &["init", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: wkp init"), "got: {stdout}");
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    assert!(
        entries.is_empty(),
        "wkp init --help must not create any files/directories, found: {entries:?}"
    );
}

#[test]
fn import_help_prints_usage_and_creates_no_bogus_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_in(dir.path(), &["import", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: wkp import"), "got: {stdout}");
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    assert!(
        entries.is_empty(),
        "wkp import --help must not create any files/directories, found: {entries:?}"
    );
}

/// The other half of the same bug: an unrecognized flag that *isn't*
/// `--help` must error, not silently become the path.
#[test]
fn init_rejects_an_unrecognized_flag_instead_of_treating_it_as_a_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let output = run_in(dir.path(), &["init", "--not-a-real-flag"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unrecognized argument: --not-a-real-flag"),
        "got: {stderr}"
    );
    assert!(
        !dir.path().join("--not-a-real-flag").exists(),
        "must not have created a directory literally named after the flag"
    );
}

/// Every subcommand's own `--help`/`-h` (checked in `dispatch` before
/// each hand-rolled parser sees the args, per `wants_help`'s doc
/// comment) must print real usage text and exit 0 -- not the bare
/// `unrecognized argument: --help` every parser used to produce.
#[test]
fn every_subcommand_help_flag_prints_usage_and_exits_zero() {
    let cases: &[(&[&str], &str)] = &[
        (&["search", "--help"], "Usage: wkp search"),
        (&["search", "-h"], "Usage: wkp search"),
        (&["context", "--help"], "Usage: wkp context"),
        (&["traverse", "--help"], "Usage: wkp traverse"),
        (&["index", "--help"], "Usage: wkp index"),
        (&["materialize", "--help"], "Usage: wkp materialize"),
        (&["remember", "--help"], "Usage: wkp remember"),
        (&["promote", "--help"], "Usage: wkp promote"),
        (&["forget", "--help"], "Usage: wkp forget"),
        (&["purge", "--help"], "Usage: wkp purge"),
        (&["hooks", "--help"], "Usage: wkp hooks"),
        (
            &["resolve-conflicts", "--help"],
            "Usage: wkp resolve-conflicts",
        ),
        (&["sync", "--help"], "Usage: wkp sync"),
        (&["sync", "status", "--help"], "Usage: wkp sync"),
        (&["bundle", "--help"], "Usage: wkp bundle"),
        (&["bundle", "export", "--help"], "Usage: wkp bundle"),
        (&["bundle", "import", "--help"], "Usage: wkp bundle"),
        (&["hub", "--help"], "Usage: wkp hub"),
        (&["hub", "register", "--help"], "Usage: wkp hub"),
        (&["wkpd", "--help"], "Usage: wkp wkpd"),
        (&["usage", "--help"], "Usage: wkp usage"),
    ];
    let dir = tempfile::tempdir().expect("tempdir");
    for (args, expected) in cases {
        let output = run_in(dir.path(), args);
        assert_eq!(output.status.code(), Some(0), "wkp {args:?}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains(expected), "wkp {args:?}: got {stdout}");
    }
    let entries: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|e| e.expect("dir entry").file_name())
        .collect();
    assert!(
        entries.is_empty(),
        "no --help invocation should create any files/directories, found: {entries:?}"
    );
}

/// Generalizes the `init`/`import` bug class beyond the two subcommands
/// that actually had it: an unrecognized flag, on ANY subcommand, must
/// be rejected (nonzero exit) rather than silently treated as positional
/// data that could create a bogus file or directory. This is the
/// property that was missing specifically for `init`/`import` (no
/// parser at all); every other subcommand already had it, but nothing
/// asserted it would *stay* true if a future subcommand were added the
/// same way `init`/`import` originally were -- without their own
/// `starts_with('-')` guard.
///
/// `merge-driver` and `filter` are deliberately excluded: they're git-
/// invoked plumbing (gitattributes(5)/merge-driver protocol), not a
/// human-facing subcommand, and `filter` reads stdin to completion
/// before erroring, which isn't safe to drive from a bare subprocess
/// call here.
#[test]
fn every_subcommand_rejects_an_arbitrary_unrecognized_flag_without_side_effects() {
    let bogus = "--this-flag-does-not-exist-xyz123";
    let cases: &[&[&str]] = &[
        &["init", bogus],
        &["import", bogus],
        &["search", bogus],
        &["context", bogus],
        &["traverse", bogus],
        &["index", bogus],
        &["materialize", bogus],
        &["remember", bogus],
        &["promote", bogus],
        &["forget", bogus],
        &["purge", bogus],
        &["hooks", bogus],
        &["resolve-conflicts", bogus],
        &["sync", bogus],
        &["sync", "status", bogus],
        &["bundle", bogus],
        &["bundle", "export", bogus],
        &["bundle", "import", bogus],
        &["hub", bogus],
        &["hub", "register", bogus],
        &["wkpd", bogus],
        &["usage", bogus],
    ];
    for args in cases {
        let dir = tempfile::tempdir().expect("tempdir");
        let output = run_in(dir.path(), args);
        assert_ne!(
            output.status.code(),
            Some(0),
            "wkp {args:?} must not succeed on an unrecognized flag"
        );
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .expect("read_dir")
            .map(|e| e.expect("dir entry").file_name())
            .collect();
        assert!(
            entries.is_empty(),
            "wkp {args:?} must not create any files/directories, found: {entries:?}"
        );
    }
}

#[test]
fn unknown_subcommand_names_it_and_still_shows_usage() {
    let output = run(&["definitely-not-a-real-subcommand"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown subcommand 'definitely-not-a-real-subcommand'"),
        "got: {stderr}"
    );
    assert!(stderr.contains("Usage: wkp <COMMAND>"), "got: {stderr}");
}
