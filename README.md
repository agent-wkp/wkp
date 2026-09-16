# agent-wkp — Workspace Knowledge Protocol

`agent-wkp` is being rewritten from a Python CLI into `wkp`, a single static
Rust binary that gives any agentic harness durable, cross-machine,
cross-harness memory: a git repository of markdown files, a derived SQLite
FTS5 index, and optional sync to a hosted hub.

This is that rewrite, and (per ADR-0013) is now `main`'s own tree — `v2-rust`
is retired. M0 through M5 are done; M6 (hardening and release) is mostly
done: Linux Landlock/seccomp sandboxing, cosign-signed reproducible
releases with SLSA provenance and SBOM, a Homebrew tap (#175), and an
enforcing OpenSSF Scorecard gate all exist and are exercised in CI. Still
open: macOS process sandboxing (#171) and self-update signature
verification (#177) — see `docs/plan/milestones.md` for exactly what each
milestone's own exit criterion holds and what's still open.

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

macOS Intel and Linux arm64 aren't built yet — see `docs/plan/milestones.md`'s
M6 task 3 for why (a real, still-open build-reproducibility gap for the
Linux arm64 target via `cross`, and macOS Intel simply not attempted yet).

- [`CLAUDE.md`](CLAUDE.md) — the working agreement for anyone (human or
  agent) contributing code on this branch.
- [`docs/design/wkp-hub-design-v0.1.md`](docs/design/wkp-hub-design-v0.1.md) —
  the authoritative design.
- [`docs/plan/milestones.md`](docs/plan/milestones.md) — milestones and
  per-milestone task lists.
- [`AGENTS.md`](AGENTS.md) — how an agent uses the `wkp` CLI.

## The previous Python implementation

The original Python `agent-wkp` (progressive-disclosure knowledge index with
BM25/semantic search over SQLite) is frozen at the [`v0-python`
tag](https://github.com/williamcaban/agent-wkp/tree/v0-python) and on the
`main` branch's history. It is not maintained going forward; this rewrite
supersedes it.

## License

Apache 2.0 — see [LICENSE](LICENSE).
