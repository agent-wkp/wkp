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

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_wkp"))
        .args(args)
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
