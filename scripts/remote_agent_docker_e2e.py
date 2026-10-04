#!/usr/bin/env python3
"""Controller and deterministic Responses mock for Docker remote-agent acceptance."""

from __future__ import annotations

import argparse
from http.server import ThreadingHTTPServer
import json
from pathlib import Path
import socket
import struct
import sys
import time
from typing import Any

from remote_agent_pairing_e2e import Recorder
from remote_agent_pairing_e2e import ResponsesHandler
from remote_agent_pairing_e2e import SOURCE_MARKER
from remote_agent_pairing_e2e import _state_read
from remote_agent_pairing_e2e import _state_update
from session_script_sdk import RpcError
from session_script_sdk import SessionScriptClient


MAX_DISCOVERY_PACKET_BYTES = 8 * 1024
MAX_SOURCE_EVENTS = 64


def _record_source_event(state_file: Path, event: dict[str, object]) -> None:
    path = state_file.with_name("source-controller-events.jsonl")
    lines = path.read_text(encoding="utf-8").splitlines() if path.exists() else []
    if len(lines) >= MAX_SOURCE_EVENTS:
        return
    path.write_text(
        "\n".join((*lines, json.dumps(event, separators=(",", ":")))) + "\n",
        encoding="utf-8",
    )


def run_mock(args: argparse.Namespace) -> int:
    args.request_log.touch()
    handler = type("DockerResponsesHandler", (ResponsesHandler,), {})
    handler.role = args.role
    handler.request_log = Recorder(args.request_log)
    handler.state_file = args.state_file
    server = ThreadingHTTPServer(("0.0.0.0", 0), handler)
    args.port_file.write_text(str(server.server_port), encoding="utf-8")
    server.serve_forever()
    return 0


def _read_exact(connection: socket.socket, size: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < size:
        chunk = connection.recv(size - len(chunks))
        if not chunk:
            raise RuntimeError("discovery relay connection closed")
        chunks.extend(chunk)
    return bytes(chunks)


def _read_frame(connection: socket.socket) -> bytes:
    size = struct.unpack("!I", _read_exact(connection, 4))[0]
    if not 1 <= size <= MAX_DISCOVERY_PACKET_BYTES:
        raise RuntimeError("discovery relay frame exceeds the packet limit")
    return _read_exact(connection, size)


def _write_frame(connection: socket.socket, payload: bytes) -> None:
    if not 1 <= len(payload) <= MAX_DISCOVERY_PACKET_BYTES:
        raise RuntimeError("discovery relay payload exceeds the packet limit")
    connection.sendall(struct.pack("!I", len(payload)) + payload)


def discovery_relay_server(args: argparse.Namespace) -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((args.tcp_host, args.tcp_port))
        listener.listen()
        while True:
            connection, _ = listener.accept()
            with connection, socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as udp:
                connection.settimeout(args.timeout)
                payload = _read_frame(connection)
                udp.settimeout(args.timeout)
                udp.sendto(payload, (args.udp_host, args.udp_port))
                response, _ = udp.recvfrom(MAX_DISCOVERY_PACKET_BYTES)
                _write_frame(connection, response)


def discovery_relay_client(args: argparse.Namespace) -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((args.udp_host, args.udp_port))
        while True:
            payload, address = listener.recvfrom(MAX_DISCOVERY_PACKET_BYTES)
            with socket.create_connection(
                (args.tcp_host, args.tcp_port), timeout=args.timeout
            ) as connection:
                connection.settimeout(args.timeout)
                _write_frame(connection, payload)
                listener.sendto(_read_frame(connection), address)


def completed_task_result_is_valid(value: object) -> bool:
    if not isinstance(value, dict):
        return False
    events = value.get("events")
    if (
        value.get("state") != "completed"
        or value.get("resultStatus") != "completed"
        or value.get("stopReason") != "terminal"
        or not isinstance(events, list)
        or not events
        or events[-1] != {"type": "terminal", "status": "completed"}
    ):
        return False
    return all(
        isinstance(value.get(key), str) and bool(value[key])
        for key in ("operation", "threadId", "turnId")
    )


def cancellation_terminal_is_valid(value: object) -> bool:
    return (
        isinstance(value, dict)
        and value.get("state") == "cancelled"
        and value.get("events") == [{"type": "terminal", "status": "cancelled"}]
        and value.get("stopReason") == "terminal"
    )


def source_controller(args: argparse.Namespace) -> int:
    client = SessionScriptClient.connect_unix_socket(args.socket, args.timeout)
    try:
        client.set_server_request_handler(lambda message: _approve(message))
        _record_source_event(args.state_file, {"event": "initializing"})
        client.initialize("docker-remote-source", "Docker remote source", "0.1.0")
        _record_source_event(args.state_file, {"event": "initialized"})
        _record_source_event(args.state_file, {"event": "startingThread"})
        response = client.request("thread/start", {"cwd": args.cwd})
        thread = response.get("thread")
        if not isinstance(thread, dict) or not isinstance(thread.get("id"), str):
            raise RuntimeError("thread/start did not return a thread id")
        _state_update(args.state_file, sourceThreadId=thread["id"])
        _record_source_event(args.state_file, {"event": "threadStarted"})
        turn_response = client.request(
            "turn/start",
            {
                "threadId": thread["id"],
                "input": [{"type": "text", "text": SOURCE_MARKER}],
            },
        )
        _record_source_event(
            args.state_file,
            {
                "event": "turnStarted",
                "turnStatus": (
                    turn_response.get("turn", {}).get("status")
                    if isinstance(turn_response.get("turn"), dict)
                    else None
                ),
            },
        )
        deadline = time.monotonic() + args.wait_timeout
        while time.monotonic() < deadline:
            try:
                message = client.receive_message()
            except socket.timeout:
                continue
            _record_source_event(
                args.state_file,
                {
                    "event": "notification",
                    "method": message.get("method"),
                    "turnStatus": (
                        message.get("params", {}).get("turn", {}).get("status")
                        if isinstance(message.get("params"), dict)
                        and isinstance(message["params"].get("turn"), dict)
                        else None
                    ),
                },
            )
            client.handle_message(message)
            if message.get("method") != "turn/completed":
                continue
            params = message.get("params")
            turn = params.get("turn") if isinstance(params, dict) else None
            if isinstance(turn, dict) and turn.get("status") != "completed":
                raise RuntimeError(f"source turn failed: {turn}")
            state = _state_read(args.state_file)
            required = (
                state.get("grantObserved") is True,
                cancellation_terminal_is_valid(state.get("cancellationTerminal")),
                state.get("targetInterrupted") is True,
                completed_task_result_is_valid(state.get("completedTaskResult")),
            )
            if all(required):
                _state_update(args.state_file, controllerPassed=True)
                return 0
        raise RuntimeError("source turn did not reach cancelled target terminal state")
    finally:
        client.close()


def _approve(message: dict[str, Any]) -> dict[str, object]:
    params = message.get("params")
    if message.get("method") != "item/extensionInteraction/request" or not isinstance(
        params, dict
    ):
        raise RuntimeError(f"unexpected app-server request: {message.get('method')!r}")
    surface = params.get("surface")
    actions = surface.get("actions") if isinstance(surface, dict) else None
    action = next(
        (
            item.get("id")
            for item in actions
            if isinstance(item, dict)
            and item.get("id") in {"approve-session", "approve", "accept"}
        ),
        None,
    )
    if action is None:
        raise RuntimeError("remote extension request has no action")
    return {
        "extensionId": params.get("extensionId"),
        "interactionId": params.get("interactionId"),
        "continuation": params.get("continuation"),
        "stateRevision": params.get("stateRevision"),
        "outcome": "accepted",
        "action": {"id": action},
        "values": {},
    }


def target_observer(args: argparse.Namespace) -> int:
    deadline = time.monotonic() + args.wait_timeout
    while time.monotonic() < deadline:
        completed = _state_read(args.state_file).get("completedTaskResult")
        if completed is not None and not completed_task_result_is_valid(completed):
            raise RuntimeError("completed task result evidence is malformed")
        if completed_task_result_is_valid(completed):
            thread_id = _state_read(args.state_file).get("targetThreadId")
            if not isinstance(thread_id, str) or not thread_id:
                raise RuntimeError("target thread id was missing after completed task")
            break
        time.sleep(0.05)
    else:
        raise RuntimeError("target completed task result was not published")
    client = SessionScriptClient.connect_unix_socket(args.socket, args.timeout)
    try:
        client.set_server_request_handler(lambda message: _approve(message))
        client.initialize("docker-remote-target", "Docker remote target", "0.1.0")
        client.request("thread/resume", {"threadId": thread_id})
        _state_update(args.state_file, targetObserverSubscribed=True)
        while time.monotonic() < deadline:
            try:
                message = client.receive_message()
            except socket.timeout:
                continue
            client.handle_message(message)
            if message.get("method") != "turn/completed":
                continue
            params = message.get("params")
            turn = params.get("turn") if isinstance(params, dict) else None
            if isinstance(turn, dict) and turn.get("status") == "interrupted":
                _state_update(args.state_file, targetInterrupted=True)
                return 0
        raise RuntimeError("target turn did not report interrupted")
    finally:
        client.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    mock = commands.add_parser("mock")
    mock.add_argument("--role", choices=("source", "target"), required=True)
    mock.add_argument("--port-file", type=Path, required=True)
    mock.add_argument("--request-log", type=Path, required=True)
    mock.add_argument("--state-file", type=Path, required=True)
    mock.set_defaults(run=run_mock)
    relay_server = commands.add_parser("discovery-relay-server")
    relay_server.add_argument("--tcp-host", default="0.0.0.0")
    relay_server.add_argument("--tcp-port", type=int, required=True)
    relay_server.add_argument("--udp-host", default="127.0.0.1")
    relay_server.add_argument("--udp-port", type=int, required=True)
    relay_server.add_argument("--timeout", type=float, default=5.0)
    relay_server.set_defaults(run=discovery_relay_server)
    relay_client = commands.add_parser("discovery-relay-client")
    relay_client.add_argument("--udp-host", default="127.0.0.1")
    relay_client.add_argument("--udp-port", type=int, required=True)
    relay_client.add_argument("--tcp-host", default="127.0.0.1")
    relay_client.add_argument("--tcp-port", type=int, required=True)
    relay_client.add_argument("--timeout", type=float, default=5.0)
    relay_client.set_defaults(run=discovery_relay_client)
    for name, function in (
        ("source-controller", source_controller),
        ("target-observer", target_observer),
    ):
        command = commands.add_parser(name)
        command.add_argument("--socket", type=Path, required=True)
        if name == "source-controller":
            command.add_argument("--cwd", required=True)
        command.add_argument("--state-file", type=Path, required=True)
        command.add_argument("--timeout", type=float, default=0.25)
        command.add_argument("--wait-timeout", type=float, default=120.0)
        command.set_defaults(run=function)
    args = parser.parse_args()
    try:
        return args.run(args)
    except (OSError, RpcError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        print(f"docker remote-agent E2E failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
