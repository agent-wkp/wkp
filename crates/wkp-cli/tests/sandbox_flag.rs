//! `wkp --sandbox <backend> <COMMAND> [ARGS]` dispatch (ADR-0015, issue
//! #225). Covers the parts reachable without a real `podman` install --
//! the missing-podman path and the flag/backend/command validation --
//! against the real compiled binary, same reasoning as
//! `usage_text.rs`: `main`'s dispatch reads real `std::env::args()` and
//! calls `std::process::exit`, which a unit test can't safely exercise
//! in-process.
//!
//! Exercising a real `podman run` end to end needs a real podman
//! install and a built image (`WKP_SANDBOX_IMAGE` override) -- covered
//! separately in CI, not here, per CLAUDE.md's rule against combining
//! `.github/workflows/*` changes with feature-code PRs.

use std::process::Command;

fn run_without_podman(args: &[&str]) -> std::process::Output {
    // Empty PATH: guarantees `podman --version` cannot be found,
    // regardless of whether the machine actually running this test
    // suite has podman installed (CI's hub-integration job does).
    Command::new(env!("CARGO_BIN_EXE_wkp"))
        .args(args)
        .env("PATH", "")
        .output()
        .expect("run wkp")
}

#[test]
fn sandbox_podman_reports_missing_podman_clearly() {
    let output = run_without_podman(&["--sandbox", "podman", "index"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("podman not found"),
        "expected a clear podman-missing message, got: {stderr}"
    );
}

#[test]
fn sandbox_unknown_backend_is_rejected() {
    let output = run_without_podman(&["--sandbox", "docker", "index"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown --sandbox backend 'docker'"),
        "got: {stderr}"
    );
    assert!(
        stderr.contains("podman"),
        "should name the supported backend: {stderr}"
    );
}

#[test]
fn sandbox_without_backend_errors() {
    let output = run_without_podman(&["--sandbox"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--sandbox requires a backend"),
        "got: {stderr}"
    );
}

#[test]
fn sandbox_podman_without_command_errors() {
    let output = run_without_podman(&["--sandbox", "podman"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--sandbox podman requires a command"),
        "got: {stderr}"
    );
}
