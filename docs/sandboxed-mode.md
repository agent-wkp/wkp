# Running sandboxed

## Linux: on by default, no flag needed

`wkp index`/`remember`/`promote`/`forget` are Landlock+seccomp-sandboxed
automatically on Linux (design 7.5, issue #170): filesystem writes are
restricted to your resolved store path (covering the index and git's
object directory for free, since both live under it), and a seccomp
denylist blocks a short list of syscalls no normal `wkp` operation has a
legitimate reason to call (`ptrace`, `mount`, module loading, and
others). Read-only commands (`search`/`context`/`materialize`) are
untouched — nothing to protect on the write side, and the latency budget
for that hot path doesn't have room for extra syscalls.

Best-effort by design: an older or unsupported kernel logs a warning to
stderr and continues rather than refusing to run.

## macOS: a documented limitation, not a native mechanism

Running `wkp` directly on macOS gets the OS's ordinary per-app
protections, not Linux's write-restriction guarantee. This was a
deliberate decision (ADR-0015), not an oversight: the only real native
option, Seatbelt (`sandbox-exec`/`sandbox_init`), has a private,
undocumented, deprecated self-apply API with no well-vetted Rust
wrapper — and the remaining path (re-exec'ing under the external
`sandbox-exec` binary) is exactly the self-invocation bug class
`CLAUDE.md` documents a real prior incident for (a self-reinforcing
fork loop that exhausted a machine's memory and process table).

## The alternative: `--sandbox podman`

```bash
wkp --sandbox podman index
wkp --sandbox podman search "topic"
```

Re-execs the given subcommand inside a container instead of natively —
available on macOS (where it's the only isolation option) and on Linux
too, if you want it for a workspace you don't fully trust beyond the
default Landlock/seccomp restriction.

**Requirements:** a working `podman` on `PATH` (on macOS, a running
`podman machine`).

**Image resolution:** pulls `ghcr.io/<owner>/wkp:v<version>-sandbox` by
default, matching your binary's own version. Set `WKP_SANDBOX_IMAGE` to
point at a different image instead — the documented way to test against
a locally built image, or to pin a specific version:

```bash
podman build -f deploy/Containerfile.sandbox -t my-wkp-sandbox .
WKP_SANDBOX_IMAGE=my-wkp-sandbox wkp --sandbox podman index
```

The default image is **not** the same one `deploy/Containerfile`
publishes for general distribution — that one is built `FROM scratch`
to prove the CLI binary's own static-link property, and has no `git`
binary in it at all, so it fails immediately for anything past
`--version`. `deploy/Containerfile.sandbox` is Fedora-minimal plus
`git`, purpose-built for this.

**Why this isn't the self-invocation risk ADR-0015 rejected:** the
container's own entrypoint invokes `wkp <command>` directly, once, and
`--sandbox` is never passed back in — there's no argument sequence that
leads to a second re-exec. Structurally different from a native
`sandbox-exec` re-exec, not a mitigation of the same mechanism.

## Reference

- ADR-0015 in [`docs/adr/`](adr/) — the full decision record for why
  native macOS sandboxing was rejected in favor of this.
- [`docs/design/wkp-hub-design-v0.1.md`](design/wkp-hub-design-v0.1.md)
  section 7.5 — the threat model this all sits under.
- [`deploy/Containerfile.sandbox`](../deploy/Containerfile.sandbox) —
  the image definition itself.
