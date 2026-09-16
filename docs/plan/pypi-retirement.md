# PyPI retirement record (issue #4)

Records what was actually done on PyPI for the `agent-wkp` project's
retirement -- the human-only half of issue #4's acceptance criteria
(publishing/yanking need the account; this document is the durable
record of that having happened, per the issue's own requirement).

## What was published

- **`0.3.0`**, published 2026-09-16 (William, via `twine upload` from
  `tools/pypi-tombstone/`, verified reproducible by this session before
  upload: built, installed in a scratch venv, confirmed the `wkp`
  console script prints the retirement message to stderr and exits 2).
  Carries `Development Status :: 7 - Inactive`; long description and
  the printed message both point to GitHub Releases and the design
  doc. This is the version an unpinned `pip install agent-wkp` now
  resolves to.

## What was yanked

- **`0.1.0`** -- yanked 2026-09-16, reason: "retired in favor of rust
  binary"
- **`0.2.0`** -- yanked 2026-09-16, reason: "retired in favor of rust
  binary"

Yanking (not deleting) keeps both installable when explicitly pinned
(`pip install agent-wkp==0.1.0`) while excluding them from any
unpinned resolve, which now lands on `0.3.0`'s tombstone instead.

`0.3.0` itself is deliberately **not** yanked -- it is the intended
default resolution target, not a release to steer people away from.

## Why the name was kept, not deleted

Deleting the `agent-wkp` PyPI project name would make it immediately
re-registrable by anyone else. Every prior installer of the real
package (`0.1.0`/`0.2.0`) would then be one `pip install --upgrade` away
from running an attacker-controlled package under a name they already
trust -- a package-takeover vector affecting everyone who ever
installed the original tool, not just new users. Keeping the name
occupied by a harmless tombstone (prints a message, exits 2, does
nothing else) closes that vector permanently, at the cost of one
retired name on PyPI. This also keeps the door open for a future wheel
that bundles the compiled Rust binary, if that ever becomes the
preferred distribution channel for `wkp` (not currently planned --
GitHub Releases, Homebrew and an OCI image are M6's actual distribution
channels, design 9.6).

## Verification

```
$ curl -s https://pypi.org/pypi/agent-wkp/json | jq '.releases | to_entries[] | {version: .key, yanked: (.value[0].yanked // false)}'
{"version": "0.1.0", "yanked": true}
{"version": "0.2.0", "yanked": true}
{"version": "0.3.0", "yanked": false}
```

## Still open

Project-settings hardening (issue #4's third PyPI checklist item: 2FA
required, trusted publisher/OIDC configured or publishing disabled,
long-lived API tokens deleted) is not externally verifiable by this
session -- PyPI's public JSON API doesn't expose account security
settings. Confirm by hand in the project's PyPI settings page; not
tracked as blocking here since it doesn't change what an outside
installer sees.
