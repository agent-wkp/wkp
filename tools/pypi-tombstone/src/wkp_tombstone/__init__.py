"""Tombstone package for the retired PyPI `agent-wkp` project.

Ships no functionality. `wkp` (the console entry point this package
registers) prints a message pointing to the real, Rust-based
successor's distribution channels and exits non-zero -- so anyone who
runs a pinned old install command that happens to resolve to this
release finds out immediately, rather than silently getting a package
that does nothing.

See ../../README.md (this same directory's own README, packaged as the
long description) for the full message and links; kept in sync by hand
since this is a one-shot tombstone release, not a maintained package.
"""

import sys

MESSAGE = (
    "agent-wkp (Python) is retired. It has moved to a Rust binary, wkp.\n"
    "Install it from GitHub Releases: "
    "https://github.com/williamcaban/agent-wkp/releases\n"
    "Design doc: "
    "https://github.com/williamcaban/agent-wkp/blob/main/docs/design/wkp-hub-design-v0.1.md\n"
    "The last working Python implementation remains at the v0-python tag, "
    "unmaintained:\n"
    "https://github.com/williamcaban/agent-wkp/tree/v0-python\n"
)


def main() -> None:
    print(MESSAGE, file=sys.stderr, end="")
    sys.exit(2)


if __name__ == "__main__":
    main()
