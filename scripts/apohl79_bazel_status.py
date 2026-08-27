#!/usr/bin/env python3
"""Emit stable Bazel workspace status for an apohl79 release build."""

import os
import re
import sys


RELEASE_VERSION_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z.+-]*$")


def main() -> int:
    release_version = os.environ.get("CODEX_RELEASE_VERSION", "")
    if not RELEASE_VERSION_RE.fullmatch(release_version):
        print(
            "CODEX_RELEASE_VERSION must be set to a valid release version.",
            file=sys.stderr,
        )
        return 1

    print(f"STABLE_CODEX_RELEASE_VERSION {release_version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
