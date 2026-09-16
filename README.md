# agent-wkp — Workspace Knowledge Protocol

`agent-wkp` is being rewritten from a Python CLI into `wkp`, a single static
Rust binary that gives any agentic harness durable, cross-machine,
cross-harness memory: a git repository of markdown files, a derived SQLite
FTS5 index, and optional sync to a hosted hub.

This is that rewrite, and (per ADR-0013) is now `main`'s own tree — `v2-rust`
is retired. M0 through M5 are done; M6 (hardening and release) is mostly
done: Linux Landlock/seccomp sandboxing, cosign-signed reproducible
releases with SLSA provenance and SBOM, and an enforcing OpenSSF Scorecard
gate all exist and are exercised in CI, plus a Homebrew tap (#175, validated
by hand against a real install -- not itself part of this repo's CI, since
the tap is a separate repo). macOS process sandboxing (#171) is resolved as
a documented limitation rather than a native mechanism (ADR-0015): running
`wkp` directly on macOS gets the OS's ordinary per-app protections, not
Linux's Landlock+seccomp write-restriction guarantee; an opt-in
`--sandbox podman` flag is tracked separately (#225) for anyone who wants
that isolation on macOS. Still open: self-update signature verification
(#177) — see `docs/plan/milestones.md` for exactly what each milestone's
own exit criterion holds and what's still open.

## Install

macOS (Apple Silicon) or Linux (x86_64):

```
brew install williamcaban/wkp/wkp
```

Or download a signed binary directly from
[GitHub Releases](https://github.com/williamcaban/agent-wkp/releases) —
every release is cosign-signed (keyless, Fulcio + Rekor) with SLSA Build L3
provenance and an SBOM; see a release's own notes for verification
instructions. An OCI image (`linux/amd64`) is published alongside each
release too.

macOS Intel and Linux arm64 aren't in the release lineup yet, for different
reasons: Linux arm64 *is* built and static-linking-checked in CI
(`rust-ci.yml`'s own cross-compile job) but not released or smoke-tested,
because its `cross`-based build isn't currently reproducible (see
`docs/plan/milestones.md`'s M6 task 3); macOS Intel isn't attempted at all
yet.

- [`CLAUDE.md`](CLAUDE.md) — the working agreement for anyone (human or
  agent) contributing code on this branch.
- [`docs/design/wkp-hub-design-v0.1.md`](docs/design/wkp-hub-design-v0.1.md) —
  the authoritative design.
- [`docs/plan/milestones.md`](docs/plan/milestones.md) — milestones and
  per-milestone task lists.
- [`AGENTS.md`](AGENTS.md) — how an agent uses the `wkp` CLI (the full
  command reference; this README only covers the basics for a human
  setting things up).

## Usage

```bash
cd your-workspace
wkp init                 # creates .wkp/, git-signing config, a one-time
                          # import of any CLAUDE.md/AGENTS.md/harness memory
wkp index                # builds the SQLite FTS5 index from your .md files

wkp search "topic" --tier 2 --budget 6000   # BM25 search
wkp context "topic" --tier 2 --budget 8000  # search + linked-neighbor graph
wkp materialize --tier 0                    # writes .wkp/tier0.md
```

`wkp remember`/`wkp promote` are the write path — an agent writes proposed
memory to `inbox/` with `wkp remember` (SSH-signed). `wkp promote` moves it
into the durable tree, and normally requires a human-signed commit; the one
opt-in exception is a principal explicitly listed under `[promote] auto =
[...]` in `.wkp/config.toml` (design 7.4's documented, not-default escape
hatch for a specific trusted harness/agent identity — not a default any
agent can grant itself). See `AGENTS.md` for the full write-path walkthrough
and every other subcommand (`traverse`, `forget`, `purge`, `sync`, `bundle`,
`hub register`, `wkpd`). `wkp --help` also lists every subcommand from the
CLI itself.

## Configure your coding agent

WKP's contract with a harness is deliberately minimal (design 1.2's
"harness neutrality" goal): `wkp` on `PATH`, something that runs a shell
command at session start, and stdout. No client library, no MCP tool
quota, no daemon.

### Claude Code

```bash
wkp hooks --framework claude_code
```

prints the exact JSON to merge into `.claude/settings.local.json` (or
`.claude/settings.json` for a team-shared setting) under its `hooks` key.
It wires a `SessionStart` hook that quietly re-indexes and then `cat`s
`.wkp/tier0.md`, so Tier 0 is already in context before your first message
— no manual step needed after that.

### Codex, OpenCode, and other AGENTS.md-reading harnesses

`wkp hooks` only has a renderer for Claude Code's specific hook format
today (issue tracked in `docs/plan/milestones.md`) — Codex and OpenCode
don't have a dedicated `wkp hooks --framework` target yet. Both read a
project's `AGENTS.md` automatically, though, so the working equivalent is
adding a short instruction there:

```markdown
## WKP memory
Before starting work, run
`wkp index && wkp materialize --tier 0 && cat .wkp/tier0.md`
and treat its output as already-established project context.
```

This is the same underlying mechanism (run a command, read stdout) just
triggered by the harness's own AGENTS.md-reading convention instead of a
dedicated hook API. Any harness that can run a shell command and read a
file can participate the same way (design 1.2's harness-neutrality goal)
— swap in whatever your harness's own "run this at the start of a
session/task" mechanism is.

## Migrating from the Python version

The original Python `agent-wkp` (progressive-disclosure knowledge index with
BM25/semantic search over SQLite) is frozen at the [`v0-python`
tag](https://github.com/williamcaban/agent-wkp/tree/v0-python) and on the
`main` branch's history before the M0-4 cutover. It is not maintained going
forward; this rewrite supersedes it. The PyPI package (`pip
install agent-wkp`) now resolves to a `0.3.0` tombstone release that prints
a retirement message and exits — it carries no functionality (see
`docs/plan/pypi-retirement.md`).

**1. Remove the old package**, if you had it installed:

```bash
pip uninstall agent-wkp        # or: pipx uninstall agent-wkp
```

**2. Install the new binary** — see [Install](#install) above.

**3. Clean and reinitialize each workspace's knowledge structures.** Both
versions use the same `.wkp/` directory convention, but the Rust version's
SQLite index has a different schema (FTS5, not the Python version's own
tables) — the old `.wkp/index.db` isn't readable by the new binary. Remove
only the derived files (index, materialized tiers, device identity), not
the whole directory: `.wkp/config.toml`, if you've set one up, carries real
configuration (e.g. `[promote] auto = [...]`, an opt-in auto-promote
allowlist) rather than derived state, and deleting it would silently lose
that setting.

```bash
cd your-workspace
rm -f .wkp/index.db .wkp/tier*.md .wkp/device-id .wkp/device-identity
wkp init
wkp index
```

Your actual memory — the markdown files with OKF frontmatter that `.wkp/`
was derived from — is untouched by this; only the derived index and
materialized tier files get rebuilt. The new parser tolerates the old
version's simpler frontmatter (M1's own acceptance criteria: "tolerant of
missing or malformed frontmatter"), so existing files don't need hand
editing to be re-indexed, though the v2 fields (`scope`, `provenance`,
`confidence`, `expires`) described in the design doc are only populated
going forward by `wkp remember`/`wkp promote`, not retroactively inferred
for old content.

## License

Apache 2.0 — see [LICENSE](LICENSE).
