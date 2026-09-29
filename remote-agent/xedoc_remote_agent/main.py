"""Command-line entry point for ``xedoc-remote-agentd``."""

from __future__ import annotations

import argparse
import json
import sys
from typing import Sequence

from .daemon import RemoteAgentDaemon
from .errors import BrokerError


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    if args.command == "audit" and args.action != "export":
        parser.error("audit requires the export action")
    if args.command == "certificate" and args.action != "export":
        parser.error("certificate requires the export action")
    try:
        if args.command == "doctor":
            report = RemoteAgentDaemon.doctor(
                xedoc_home=args.xedoc_home,
                timeout=args.timeout,
                version=args.version,
                host_id=args.host_id,
            )
            print(json.dumps(report, ensure_ascii=False, separators=(",", ":")))
            return 0 if report.get("ok") is True else 1
        if args.command == "status":
            report = RemoteAgentDaemon.status(xedoc_home=args.xedoc_home)
            print(json.dumps(report, ensure_ascii=False, separators=(",", ":")))
            return 0 if report.get("status") in {"running", "stopped"} else 1
        if args.command == "shutdown":
            report = RemoteAgentDaemon.shutdown(
                xedoc_home=args.xedoc_home,
                timeout=args.timeout,
            )
            print(json.dumps(report, ensure_ascii=False, separators=(",", ":")))
            return 0 if report.get("status") == "shutdownRequested" else 1
        if args.command == "audit":
            for record in RemoteAgentDaemon.audit_export(
                xedoc_home=args.xedoc_home,
                limit=args.limit,
            ):
                print(json.dumps(record, ensure_ascii=False, separators=(",", ":")))
            return 0
        if args.command == "certificate":
            sys.stdout.write(
                RemoteAgentDaemon.certificate_export(
                    xedoc_home=args.xedoc_home,
                    host_id=args.host_id,
                )
            )
            return 0
        daemon = RemoteAgentDaemon(
            xedoc_home=args.xedoc_home,
            timeout=args.timeout,
            version=args.version,
            host_id=args.host_id,
        )
        return daemon.serve()
    except BrokerError as error:
        print(
            json.dumps(
                {
                    "service": "xedoc-remote-agentd",
                    "ok": False,
                    "error": {"code": error.code.value},
                },
                separators=(",", ":"),
            ),
            file=sys.stderr,
        )
        return 1


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="xedoc-remote-agentd")
    parser.add_argument(
        "command",
        choices=("serve", "doctor", "status", "shutdown", "audit", "certificate"),
    )
    parser.add_argument("action", nargs="?", choices=("export",))
    parser.add_argument("--xedoc-home")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--version")
    parser.add_argument("--host-id", default="host_local")
    parser.add_argument("--limit", type=int, default=1_000)
    return parser


if __name__ == "__main__":
    raise SystemExit(main())
