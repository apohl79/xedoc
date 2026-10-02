"""Command-line entry point for ``xedoc-remote-agentd``."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import subprocess
import sys
import time
from typing import Sequence

from .daemon import MAX_OWNER_IPC_BYTES
from .daemon import RemoteAgentDaemon
from .errors import BrokerError, ErrorCode
from .ipc import LocalIpcClient
from .workspaces import controller_identifier
from .workspaces import load_bootstrap_descriptor


_ENSURE_POLL_SECONDS = 0.05


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
        if args.command == "ensure":
            report = _ensure(args)
            print(json.dumps(report, ensure_ascii=False, separators=(",", ":")))
            return 0
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
        choices=(
            "serve",
            "doctor",
            "status",
            "shutdown",
            "ensure",
            "audit",
            "certificate",
        ),
    )
    parser.add_argument("action", nargs="?", choices=("export",))
    parser.add_argument("--xedoc-home")
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument("--version")
    parser.add_argument("--host-id", default="host_local")
    parser.add_argument("--limit", type=int, default=1_000)
    parser.add_argument(
        "--replace",
        action="store_true",
        help="Replace a broker owned by a different app-server target.",
    )
    parser.add_argument(
        "--restart",
        action="store_true",
        help="Reconnect the singleton broker after its app-server restarted.",
    )
    return parser


def _ensure(args: argparse.Namespace) -> dict[str, object]:
    """Ensure exactly one broker serves this process's selected controller."""

    daemon = _daemon(args)
    xedoc_home = daemon.paths.xedoc_home
    expected_controller_id = controller_identifier(
        load_bootstrap_descriptor(xedoc_home=xedoc_home)
    )
    deadline = time.monotonic() + args.timeout
    running_controller_id = _wait_for_broker_or_stop(xedoc_home, deadline)
    if running_controller_id == expected_controller_id and not args.restart:
        return {
            "status": "alreadyRunning",
            "controllerId": expected_controller_id,
        }
    if running_controller_id is not None:
        if not args.replace and not args.restart:
            raise BrokerError.conflict()
        _request_shutdown(xedoc_home, args.timeout)
        _wait_until_stopped(xedoc_home, deadline)

    _spawn_daemon(args, xedoc_home)
    _wait_until_matching_controller(
        xedoc_home,
        expected_controller_id,
        deadline,
    )
    return {"status": "started", "controllerId": expected_controller_id}


def _daemon(args: argparse.Namespace) -> RemoteAgentDaemon:
    return RemoteAgentDaemon(
        xedoc_home=args.xedoc_home,
        timeout=args.timeout,
        version=args.version,
        host_id=args.host_id,
    )


def _broker_controller_id(xedoc_home: Path, timeout: float) -> str | None:
    try:
        handshake = LocalIpcClient(
            xedoc_home=xedoc_home,
            max_message_bytes=MAX_OWNER_IPC_BYTES,
            max_result_bytes=MAX_OWNER_IPC_BYTES,
            timeout_seconds=min(timeout, 1.0),
        ).handshake()
    except BrokerError as error:
        if error.code == ErrorCode.UNAVAILABLE:
            return None
        raise
    controller_id = handshake.get("controllerId")
    if not isinstance(controller_id, str) or not controller_id:
        return ""
    return controller_id


def _wait_for_broker_or_stop(xedoc_home: Path, deadline: float) -> str | None:
    while True:
        controller_id = _broker_controller_id(
            xedoc_home,
            max(deadline - time.monotonic(), _ENSURE_POLL_SECONDS),
        )
        if controller_id is not None:
            return controller_id
        status = RemoteAgentDaemon.status(xedoc_home=xedoc_home).get("status")
        if status == "stopped":
            return None
        if time.monotonic() >= deadline:
            raise BrokerError.unavailable()
        time.sleep(_ENSURE_POLL_SECONDS)


def _request_shutdown(xedoc_home: Path, timeout: float) -> None:
    report = RemoteAgentDaemon.shutdown(
        xedoc_home=xedoc_home,
        timeout=min(timeout, 5.0),
    )
    if report.get("status") not in {"shutdownRequested", "error"}:
        raise BrokerError.unavailable()


def _wait_until_stopped(xedoc_home: Path, deadline: float) -> None:
    while True:
        if (
            _broker_controller_id(
                xedoc_home,
                max(deadline - time.monotonic(), _ENSURE_POLL_SECONDS),
            )
            is None
            and RemoteAgentDaemon.status(xedoc_home=xedoc_home).get("status")
            == "stopped"
        ):
            return
        if time.monotonic() >= deadline:
            raise BrokerError.unavailable()
        time.sleep(_ENSURE_POLL_SECONDS)


def _spawn_daemon(args: argparse.Namespace, xedoc_home: Path) -> None:
    try:
        payload = Path(sys.argv[0]).resolve(strict=True)
    except OSError as error:
        raise BrokerError.unavailable() from error
    if not payload.is_file():
        raise BrokerError.unavailable()

    command = [
        sys.executable,
        "-B",
        "-I",
        str(payload),
        "daemon",
        "serve",
        "--xedoc-home",
        str(xedoc_home),
        "--timeout",
        str(args.timeout),
        "--host-id",
        args.host_id,
    ]
    if args.version is not None:
        command.extend(("--version", args.version))
    try:
        subprocess.Popen(
            command,
            close_fds=True,
            start_new_session=True,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except OSError as error:
        raise BrokerError.unavailable() from error


def _wait_until_matching_controller(
    xedoc_home: Path,
    expected_controller_id: str,
    deadline: float,
) -> None:
    while True:
        controller_id = _broker_controller_id(
            xedoc_home,
            max(deadline - time.monotonic(), _ENSURE_POLL_SECONDS),
        )
        if controller_id == expected_controller_id:
            return
        if controller_id is not None:
            raise BrokerError.conflict()
        if time.monotonic() >= deadline:
            raise BrokerError.unavailable()
        time.sleep(_ENSURE_POLL_SECONDS)


if __name__ == "__main__":
    raise SystemExit(main())
