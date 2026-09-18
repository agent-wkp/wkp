# Migrating from the Python version

The original Python `agent-wkp` (progressive-disclosure knowledge index with
BM25/semantic search over SQLite) is frozen at the [`v0-python`
tag](https://github.com/agent-wkp/wkp/tree/v0-python) and on the
`main` branch's history before the M0-4 cutover. It is not maintained going
forward; this rewrite supersedes it. The PyPI package (`pip
install agent-wkp`) now resolves to a `0.3.0` tombstone release that prints
a retirement message and exits — it carries no functionality (see
[`docs/plan/pypi-retirement.md`](plan/pypi-retirement.md)).

**1. Remove the old package**, if you had it installed:

```bash
pip uninstall agent-wkp        # or: pipx uninstall agent-wkp
```

**2. Install the new binary** — see the [README's Install
section](../README.md#install).

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
