# ADR-0015: macOS native process sandboxing — document as a limitation, offer opt-in `--sandbox` flag instead

Status: accepted
Date: 2026-09-16
Design sections affected: 7.5 (process hardening), 8.4 (threat model table)
Related: issue #171 (scoping), issue #225 (follow-up implementation)

## Context

Design 7.5 commits Linux to Landlock (write-restriction to the resolved
store path) plus a seccomp denylist, both enforced in-process with no
`unsafe`. The same section names macOS's own equivalent as "not yet built"
and flags `sandbox-exec`/hardened-runtime entitlements as
**[unverified for the specific macOS version in use]**.

Issue #171 did that verification. It found a real fork, not an
implementation gap:

- **Hardened-runtime entitlements** (the other named option) are not a
  filesystem-restriction mechanism at all — they govern JIT, library
  validation, and notarization, not "deny writes outside path X." They
  don't fit this task's actual need regardless of enforceability.
- **Seatbelt** (`sandbox-exec`/`sandbox_init`) is the only remaining native
  mechanism. Its self-apply C API (`sandbox_init`) is private, undocumented,
  and deprecated with no committed replacement. No well-vetted, widely-used
  Rust crate wraps it safely — the options on crates.io are tiny/low-adoption,
  unlike `landlock`/`seccompiler`'s official-grade provenance — and
  `wkp-cli`'s `#![forbid(unsafe_code)]` rules out hand-rolling that FFI
  directly.
- The remaining path — re-exec'ing `wkp` under the external
  `/usr/bin/sandbox-exec` binary, safe Rust only — is exactly the
  self-invocation bug class CLAUDE.md calls out by name, citing a real prior
  incident (a self-reinforcing fork loop that exhausted a machine's memory
  and process table). `resolve_wkp_exe()` mitigates *how* the binary is
  found, not *whether* a self-re-exec under a deprecated, privately-APId
  external tool is the right mechanism to build on at all.

M6's exit criterion (`milestones.md`) currently reads as if this is a
mechanical port of the Linux implementation. It isn't. This ADR decides what
M6 actually commits to on macOS.

## Options

1. **Build the `sandbox-exec` re-exec anyway.** Matches Linux's shape
   (in-process-adjacent, default-on). Costs: builds on a deprecated, private
   API with no replacement timeline; the self-re-exec pattern is a known
   incident-causing bug class on this codebase specifically; ongoing
   maintenance risk is borne by every future macOS release, not just this
   one.
2. **Do nothing; leave the gap silently undocumented.** Rejected outright —
   contradicts design 7.5's own security framing and CLAUDE.md's "no
   half-finished implementations" stance applied to documentation as much as
   code: a reader of 7.5 would reasonably assume macOS has the same
   guarantee Linux does.
3. **Document native macOS as a known limitation now; offer an opt-in
   `--sandbox <backend>` flag as a separate, later feature, starting with
   `--sandbox podman`.** The flag re-execs the requested subcommand inside a
   container, reusing container-runtime tooling `wkp-hub` already depends on
   (already-audited isolation) rather than hand-rolling Seatbelt FFI or
   building on a deprecated private API. Not default on any platform — a
   user (or CI) opts in explicitly. Because the container's own entrypoint
   invokes `wkp <original args>` once, without `--sandbox`, there is no path
   back into a second `--sandbox` re-exec — the self-invocation risk that
   ruled out option 1 does not apply here by construction (no recursion is
   reachable, not just unlikely).

## Decision

**Option 3.** Native macOS process sandboxing is documented as a known
limitation — running `wkp` directly on macOS gets the OS's own ordinary
per-app protections, not the Landlock+seccomp write-restriction guarantee
Linux gets. No native mechanism is built to close that gap; the
`sandbox-exec` re-exec approach (option 1) is explicitly rejected, not
deferred, on the same self-invocation grounds CLAUDE.md already documents an
incident for.

Instead, issue #225 tracks an opt-in `--sandbox <backend>` flag
(`--sandbox podman` first) for anyone who wants filesystem isolation on
macOS badly enough to accept a container-runtime dependency for it. That
work is scoped separately, is not default on any platform, and — because it
touches sandbox rules — needs its own CODEOWNERS co-sign per CLAUDE.md, same
as the Linux Landlock/seccomp code did.

**Not decided here:** the concrete implementation of `--sandbox podman`
(image contents, bind-mount scope, whether it also becomes available on
Linux as an alternative to the native Landlock path) — reserved for #225's
own design pass.

## Consequences

- M6's exit criterion no longer reads as "macOS sandboxing: open task." It
  reads as "macOS: documented limitation, opt-in container-based
  alternative tracked separately" — a closed decision, not a stalled one.
  `milestones.md` and design 7.5 are updated in the same change as this ADR.
- Issue #171 is closed as **won't-fix as originally scoped** — the
  scoping work it asked for is done, and its answer is "don't build this,
  build #225 instead," not "still open."
- A macOS user running `wkp` natively today has no way to get Linux's
  filesystem-write-restriction guarantee until #225 lands; the README and
  design doc's threat-model table (8.4) should say so plainly rather than
  imply parity with Linux.
- `--sandbox podman`, when built, adds a container-runtime dependency to a
  previously daemon-free, dependency-light CLI operation — an explicit,
  opt-in cost, not one imposed on every macOS user by default. That tradeoff
  is #225's to make explicit in its own PR description, not assumed here.
