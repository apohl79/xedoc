#!/usr/bin/env python3
"""Build a signed local release package for the apohl79 Codex fork."""

import os
from pathlib import Path
import shutil
import sys


SCRIPT_DIR = Path(__file__).resolve().parent


def reexec_with_uv_if_needed() -> None:
    if sys.version_info >= (3, 10):
        return

    uv = shutil.which("uv")
    if uv is None:
        print("Error: Python 3.10+ or uv is required.", file=sys.stderr)
        raise SystemExit(1)

    os.execvp(
        uv,
        [
            uv,
            "run",
            "--frozen",
            "--project",
            str(SCRIPT_DIR),
            "python",
            str(Path(__file__).resolve()),
            *sys.argv[1:],
        ],
    )


reexec_with_uv_if_needed()

sys.path.insert(0, str(SCRIPT_DIR))

from apohl79_release import main


if __name__ == "__main__":
    raise SystemExit(main())
