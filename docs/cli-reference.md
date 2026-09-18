# CLI reference

Every `wkp` subcommand and its flags. For a narrative walkthrough of the
read/write paths and when to call what, see [`AGENTS.md`](../AGENTS.md)
(written for an agent, but equally useful to a human). This doc is the
flat reference: one entry per subcommand, what it does, exact syntax.

`wkp --help` prints the short form of this list from the CLI itself;
`wkp --version`/`-V` prints the version.

Flags shown in `[brackets]` are optional. `--path <dir>` defaults to the
current directory everywhere it appears, unless noted otherwise.

## Store setup

### `wkp init [path]`

Makes `path` (default: cwd) a git repository with a `.wkp/` directory,
wires SSH commit signing, registers the merge driver and clean/smudge
filter, and runs `wkp import` once. Idempotent — safe to re-run.

### `wkp import [path]`

One-shot, idempotent migration of pre-existing memory into
`inbox/import/`: `CLAUDE.md`/`AGENTS.md` at the store root, and every
`~/.claude/projects/*/memory/*.md` file. Runs automatically as part of
`wkp init`; call it again by hand if new source files show up later. An
existing destination file is never overwritten.

## Reading

### `wkp search <query> [--tier N] [--budget N] [-k/--limit N] [--format text|paths|json] [--path DIR]`

BM25 full-text search over `index.db`. `--budget` stops once estimated
token cost exceeds N. `--format paths` is the form to feed into a
Read-tool call.

Also accepts `--embed-url URL [--embed-model NAME] [--embed-key-file PATH]`
for opt-in hybrid search (Reciprocal Rank Fusion between BM25 and
embedding cosine similarity against an OpenAI-compatible endpoint) —
requires a binary built with the `embed` Cargo feature and embeddings
already computed via `wkp index --embed-url ...`. Never called by
default — the only network call anywhere in the read path (`search`/
`context`/`traverse`/`materialize`/`index`). `sync`, `bundle`, and
`hub register` are network operations too, by design (that's their
whole job); this flag is specifically about the read/search path never
reaching out unless you opt in.

### `wkp context <query> [--tier N] [--budget N] [-k/--limit N] [--format text|paths|json] [--path DIR]`

Same flags as `search`. Adds a graph traversal of the `refs:`/wikilink
edges from each hit, so you also get directly-linked neighbors, not just
the hits themselves.

### `wkp traverse <path> [--depth N] [--format text|paths|json] [--path DIR]`

Walks `refs:`/`[[wikilink]]` edges outward from one specific file, no
search query involved. `--depth` defaults to 2.

### `wkp materialize --tier N [--path DIR]`

Writes `.wkp/tier{N}.md` atomically (temp file + rename). `--tier` is
required — no default, deliberately, so a session-start hook never
materializes "whatever tier" by accident. This is what a `SessionStart`
hook reads for tier 0.

### `wkp index [path] [--embed-url URL] [--embed-model NAME] [--embed-key-file PATH]`

Rescans the store and updates `index.db` incrementally, using git's own
change detection to hash only what changed. Only `.md` files are
indexed. Safe to run any time; cheap after a single-file edit.

## Writing

### `wkp remember --type <type> --principal <principal> --signing-key-file <path> [--scope <scope>] [--title <title>] [--session <session>] [--path <dir>]`

Writes one SSH-signed item to `inbox/`, body read from **stdin** (never
argv). `--type`, `--principal`, and `--signing-key-file` are required.
Always lands at `confidence: proposed`, tier 2, regardless of `--type` —
nothing self-promotes to tier 0/1.

```bash
wkp remember --type knowledge --principal agent:claude --signing-key-file ~/.ssh/id_ed25519 <<'EOF'
Whatever you learned, as plain markdown.
EOF
```

### `wkp promote <inbox-path> [--to <dest-path>] --principal <principal> --signing-key-file <path> [--path <dir>]`

Moves one `inbox/` item into the durable tree with a human-signed commit.
The one opt-in exception to "human-signed": a principal explicitly listed
under `[promote] auto = [...]` in `.wkp/config.toml`.

### `wkp forget <path> --principal <p> --signing-key-file <f> [--path <dir>]`

or

### `wkp forget --device <id> --principal <p> --signing-key-file <f> [--path <dir>]`

Two operations under one verb: the first form removes one tracked item;
the second revokes a device's encryption-recipient entry and
re-encrypts every currently-tracked `visibility: private` file. Both
always require `role: human` — no auto-forget escape hatch, unlike
`promote`.

### `wkp purge <path> [--path <dir>]`

Erases a path from git history entirely, wrapping the upstream
`git-filter-repo` tool (not vendored — install it separately:
`apt install git-filter-repo` / `brew install git-filter-repo` /
`pip install git-filter-repo`).

## Sync

### `wkp sync [--remote <name>] [--path <dir>]`

Fetches and pushes this device's branch, converging with peer devices
through the merge driver.

### `wkp sync status [--path <dir>]`

Reports unresolved conflicts without changing anything.

### `wkp bundle export <output-path> [--since <ref>] [--path <dir>]`

Writes a `git bundle` for air-gapped sync — everything, or everything
since `--since`.

### `wkp bundle import <bundle-path> [--path <dir>]`

Applies a bundle produced by `bundle export` on another device.

### `wkp resolve-conflicts [path]`

Resolves modify/delete conflicts (design 6.2: a deletion always loses to
a modification) by keeping the modification and proposing the deletion
as an `inbox/` item instead of silently dropping either side.

## Hub

### `wkp hub register --hub-url <url> --tenant <slug> --ca-cert <path> [--path <dir>]`

Registers this device with a `wkp-hub` (RFC 8628 device flow), issuing
an mTLS device certificate. `--ca-cert` is the hub's own CA root
(`wkp-hub ca-cert`).

### `wkp wkpd --principal <p> --signing-key-file <path> [--path <dir>] [--remote <name>] [--socket-path <path>] [--debounce-ms <n>]`

Watch-triggered sync daemon: debounced auto-commit of local changes,
incremental re-index, opportunistic `sync`. Binds a Unix domain socket
only (never TCP), peer-UID verified on every connection — Linux only;
refuses to start elsewhere rather than running with no peer check. See
[`deploy/systemd/wkpd.service`](../deploy/systemd/wkpd.service) for a
ready-to-edit `systemd --user` unit.

## Harness integration

### `wkp hooks --framework <name>`

Prints text for a harness to apply — never writes a file itself.

- **`claude_code`** — the exact `SessionStart` hook JSON. See the
  [README](../README.md#claude-code) for the merge command.
- **`codex`, `opencode`, `hermes`, `agents_md`** — all four print the
  identical plain-text `AGENTS.md` instruction (`wkp index && wkp
  materialize --tier 0 && cat .wkp/tier0.md`). Codex and OpenCode are
  confirmed `AGENTS.md` auto-readers. Hermes Agent is too, verified
  against its own source (`agent/prompt_builder.py`,
  `NousResearch/hermes-agent`) — but with one real caveat: Hermes loads
  at most one project-context file, first match wins, in this order —
  `.hermes.md`/`HERMES.md` (walking up to the git root) → `AGENTS.md`
  chain → `CLAUDE.md` → `.cursorrules`. So `AGENTS.md` reaches Hermes
  exactly like it reaches Codex/OpenCode *unless* the project also has
  its own `.hermes.md`/`HERMES.md`, which wins instead. `agents_md` is
  the generic name for any other AGENTS.md-reading harness.

## Sandboxed execution

### `wkp --sandbox podman <command> [args...]`

Re-execs `<command>` inside a container instead of natively. See
[`docs/sandboxed-mode.md`](sandboxed-mode.md) for what this is for, what
it needs, and how it's safe from the self-invocation risk a native
macOS sandbox would have had.

## Git plumbing (not called directly)

`wkp merge-driver <ancestor> <ours> <theirs>` and `wkp filter clean|smudge`
are invoked by git itself, per its own merge-driver and clean/smudge
filter protocols (`wkp init` registers both). Not commands a human or
harness runs directly.
