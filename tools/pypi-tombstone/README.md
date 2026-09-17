# agent-wkp — retired

This package is retired. `agent-wkp` has been rewritten as `wkp`, a single
static Rust binary — it is no longer distributed via PyPI.

Install the new binary from one of:

- [GitHub Releases](https://github.com/agent-wkp/wkp/releases)
  (signed archives, SBOM, and attestations)
- Homebrew tap (in progress)
- An OCI image, published alongside GitHub Releases

The design doc for the rewrite:
<https://github.com/agent-wkp/wkp/blob/main/docs/design/wkp-hub-design-v0.1.md>

The last working Python implementation remains available, unmaintained, at
the [`v0-python` tag](https://github.com/agent-wkp/wkp/tree/v0-python)
— install from git directly (`pip install git+https://github.com/agent-wkp/wkp@v0-python`)
if you specifically need it; this PyPI project name itself is kept
(not deleted) only to prevent name-squatting, and carries no functionality
from this release on.

Running `wkp` from this package prints this message to stderr and exits
with status 2.
