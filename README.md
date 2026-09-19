# agent-wkp — Workspace Knowledge Protocol

`wkp` is a single static Rust binary that gives any agentic harness
durable, cross-machine, cross-harness memory: a git repository of
markdown files, a derived SQLite FTS5 index, and optional sync to a
hosted hub. No daemon, no client library, no MCP tool quota required.

> This repository moved from `github.com/williamcaban/agent-wkp` to
> `github.com/agent-wkp/wkp` on 2026-09-17. Old clone/remote URLs still
> redirect. The Homebrew tap moved too, to `agent-wkp/homebrew-wkp`.

## Install

macOS (Apple Silicon) or Linux (x86_64):

```bash
brew install agent-wkp/wkp/wkp
```

Or download a signed binary from [GitHub
Releases](https://github.com/agent-wkp/wkp/releases) — every release is
cosign-signed with SLSA provenance and an SBOM; see a release's own notes
to verify. See [`docs/plan/build-targets.md`](docs/plan/build-targets.md)
for the full target matrix and what isn't released yet.

## Quick start

```bash
cd your-workspace
wkp init                 # one-time: creates .wkp/, git-signing config,
                          # imports any existing CLAUDE.md/AGENTS.md
wkp index                # builds the search index from your .md files

wkp search "topic"                # BM25 search
wkp context "topic"               # search + linked context
wkp materialize --tier 0          # writes .wkp/tier0.md
```

That covers reading. Agents also write memory (`wkp remember`) for a
human to promote (`wkp promote`) — see
[`docs/cli-reference.md`](docs/cli-reference.md) for every subcommand,
or [`AGENTS.md`](AGENTS.md) for the full write-path walkthrough.
`wkp search`/`wkp context` treat the query as plain text, so
punctuation like `RHOAI 3.6` or `file-name.ext` searches literally
instead of raising a query-syntax error.

## Usage modes

What you've just done above is **local mode** — one machine, no sync,
nothing else to set up. Two more modes build on top of it:

- **[Multiple machines, via a private git repo](docs/multi-machine-sync.md)** —
  clone the same store onto more than one machine, sync through a git
  remote you already trust (a private GitHub repo, a NAS). No extra
  service required.
- **[Hub mode](docs/hub-mode.md)** — a shared, always-on service instead
  of managing your own git host: device registration and revocation
  over mTLS, per-tenant isolation. Reach for this over plain sync when
  you need revocation or multiple people sharing a tenant.

## Configure your coding agent

### Claude Code

```bash
wkp hooks --framework claude_code
```

prints the `SessionStart` hook JSON. Merge it into
`.claude/settings.local.json` (or `.claude/settings.json` for a
team-shared setting) under its `hooks` key — with `jq`, so any other
settings and hooks already there are preserved, and re-running it is
safe (won't add a duplicate entry):

```bash
mkdir -p .claude
[ -s .claude/settings.local.json ] || echo '{}' > .claude/settings.local.json
wkp hooks --framework claude_code | jq -s '
    .[0].hooks.SessionStart = ((.[0].hooks.SessionStart // []) - .[1].hooks.SessionStart + .[1].hooks.SessionStart) | .[0]
  ' .claude/settings.local.json - > /tmp/wkp-settings-merge.json \
  && mv /tmp/wkp-settings-merge.json .claude/settings.local.json
```

Tier 0 is now injected automatically before your first message, every
session.

### Codex, OpenCode, Hermes, and other AGENTS.md-reading harnesses

```bash
wkp hooks --framework codex >> AGENTS.md   # or: opencode, hermes, agents_md
```

All four print the same short instruction, appended to your project's
`AGENTS.md` — see [`docs/cli-reference.md`](docs/cli-reference.md#harness-integration)
for exactly what it says and a caveat specific to Hermes.

## Running sandboxed

Linux sandboxes `wkp index`/`remember`/`promote`/`forget` automatically,
no flag needed. On macOS, or for extra isolation on Linux,
`wkp --sandbox podman <command>` re-execs any subcommand in a container
instead — see [`docs/sandboxed-mode.md`](docs/sandboxed-mode.md) for
requirements and how it works.

## Migrating from the Python version

The Rust binary supersedes the original Python `agent-wkp`; the PyPI
package is retired. See
[`docs/migrating-from-python.md`](docs/migrating-from-python.md).

## More

- [`docs/cli-reference.md`](docs/cli-reference.md) — every subcommand
  and its flags.
- [`AGENTS.md`](AGENTS.md) — how an agent should use `wkp` day to day.
- [`docs/design/wkp-hub-design-v0.1.md`](docs/design/wkp-hub-design-v0.1.md) —
  the authoritative design.
- [`docs/plan/milestones.md`](docs/plan/milestones.md) — what's built,
  what's still open, and why.
- [`CLAUDE.md`](CLAUDE.md) — the working agreement for anyone (human or
  agent) contributing code to this repo.

## License

Apache 2.0 — see [LICENSE](LICENSE).
