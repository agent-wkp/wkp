# agent-wkp — retired

This package is retired. `agent-wkp` has been rewritten as `wkp`, a single
static Rust binary — it is no longer distributed via PyPI.

Install the new binary from one of:

- [GitHub Releases](https://github.com/williamcaban/agent-wkp/releases)
  (signed archives, SBOM, and attestations)
- Homebrew tap (coming with the rewrite's M6 milestone)
- An OCI image (coming with the rewrite's M6 milestone)

The design doc for the rewrite:
<https://github.com/williamcaban/agent-wkp/blob/v2-rust/docs/design/wkp-hub-design-v0.1.md>

The last working Python implementation remains available, unmaintained, at
the [`v0-python` tag](https://github.com/williamcaban/agent-wkp/tree/v0-python)
— install from git directly (`pip install git+https://github.com/williamcaban/agent-wkp@v0-python`)
if you specifically need it; this PyPI project name itself is kept
(not deleted) only to prevent name-squatting, and carries no functionality
from this release on.

Running `wkp` from this package prints this message to stderr and exits
with status 2.
