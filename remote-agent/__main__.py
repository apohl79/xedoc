"""Dispatch the bundled remote-agent zipapp roles."""

from __future__ import annotations

import sys


def main() -> int:
    if len(sys.argv) < 2:
        print("usage: remote-agent.pyz {extension|daemon} [args...]", file=sys.stderr)
        return 2
    role, *arguments = sys.argv[1:]
    if role == "extension":
        from xedoc_remote_agent.extension import main as extension_main

        return extension_main(arguments)
    if role == "daemon":
        from xedoc_remote_agent.main import main as daemon_main

        return daemon_main(arguments or ["serve"])
    print(f"unknown remote-agent role: {role}", file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
