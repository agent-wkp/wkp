//! M6-1 (issue #170): real end-to-end proof that `sandbox.rs`'s two
//! functions actually do what they claim, run as real subprocesses of
//! the actually-compiled `wkp` binary via its two hidden, undocumented
//! `__sandbox-self-test-*` subcommands (`main.rs`'s own doc comment on
//! them explains why they exist and what they deliberately don't
//! prove: `wkp-cli`'s `#![forbid(unsafe_code)]` rules out an `unsafe`
//! FFI call to `ptrace` inside the crate itself, so the claim that the
//! *specific* denied syscalls actually fail was verified by hand
//! instead, in a standalone throwaway program -- see `sandbox.rs`'s own
//! doc comment on `restrict_dangerous_syscalls`).
//!
//! Linux-only, like the functions under test: on `aarch64-apple-darwin`
//! both hidden subcommands are still reachable (the underlying
//! functions are no-ops there) but there's nothing meaningful to assert
//! about restriction taking effect, so this test doesn't run there.

#![cfg(target_os = "linux")]

use std::process::Command;

fn wkp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_wkp")
}

#[test]
fn write_sandbox_allows_inside_denies_outside() {
    let allowed = tempfile::tempdir().expect("tempdir");
    let denied = tempfile::tempdir().expect("tempdir");

    let output = Command::new(wkp_bin())
        .args([
            "__sandbox-self-test-write",
            allowed.path().to_str().expect("utf8 path"),
            denied.path().to_str().expect("utf8 path"),
        ])
        .output()
        .expect("run wkp __sandbox-self-test-write");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected the write sandbox to allow the allowed dir and deny the denied one; \
         stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(stdout.contains("INSIDE_WRITE_OK=true"), "stdout={stdout:?}");
    assert!(
        stdout.contains("OUTSIDE_WRITE_DENIED=true"),
        "stdout={stdout:?}"
    );

    // Independently confirmed from the test process itself, not just
    // trusting the subprocess's own self-report: the file really is
    // there, and really isn't there.
    assert!(allowed.path().join("ok.txt").exists());
    assert!(!denied.path().join("nope.txt").exists());
}

#[test]
fn syscall_filter_leaves_ordinary_operations_working() {
    let probe_dir = tempfile::tempdir().expect("tempdir");

    let output = Command::new(wkp_bin())
        .args([
            "__sandbox-self-test-syscalls",
            probe_dir.path().to_str().expect("utf8 path"),
        ])
        .output()
        .expect("run wkp __sandbox-self-test-syscalls");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected an ordinary write to still succeed after the seccomp filter is applied \
         (a catastrophically backwards filter would fail this); stdout={stdout:?} stderr={stderr:?}"
    );
    assert!(
        stdout.contains("ORDINARY_WRITE_OK=true"),
        "stdout={stdout:?}"
    );
}

/// Regression test for a real bug the two tests above couldn't catch:
/// both only ever drive the hidden `__sandbox-self-test-*` subcommands,
/// neither of which spawns a real `git` subprocess. `wkp index` does,
/// via `wkp-git`, immediately after `apply_write_sandbox` runs -- and
/// on a kernel that actually enforces the restriction, every one of
/// those `git` calls used to die with "could not open '/dev/null' for
/// reading and writing" (git's own startup-time `sanitize_stdfds()`
/// opens `/dev/null` read+write unconditionally, regardless of
/// subcommand, and that path was outside the allowed set). Fixed by
/// including `/dev/null` in `apply_write_sandbox`'s allowed paths (see
/// its own doc comment in `main.rs`) -- this test exercises the real
/// compiled binary's real `index` subcommand against a real git repo,
/// the actual path that was broken, not just the sandbox primitives in
/// isolation.
#[test]
fn index_succeeds_under_the_write_sandbox_against_a_real_git_repo() {
    let store = tempfile::tempdir().expect("tempdir");

    let init = Command::new(wkp_bin())
        .arg("init")
        .arg(store.path())
        .output()
        .expect("run wkp init");
    assert!(
        init.status.success(),
        "wkp init failed: {}",
        String::from_utf8_lossy(&init.stderr)
    );

    std::fs::create_dir_all(store.path().join("projects/demo")).expect("mkdir");
    std::fs::write(
        store.path().join("projects/demo/note.md"),
        "---\ntype: instruction\nscope: project\n---\nkeyword: zephyr\n",
    )
    .expect("write fixture note");

    let index = Command::new(wkp_bin())
        .arg("index")
        .arg(store.path())
        .output()
        .expect("run wkp index");
    let stderr = String::from_utf8_lossy(&index.stderr);
    assert!(
        index.status.success(),
        "wkp index failed under the write sandbox (this is exactly the /dev/null \
         regression): {stderr}"
    );
    assert!(
        store.path().join(".wkp/index.db").exists(),
        "index.db should exist after a successful index"
    );
}
