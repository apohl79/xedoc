#!/usr/bin/env python3
"""Local deterministic coordinator-to-coordinator pairing E2E controller.

Run two local Responses mocks and then one controller from a tmux wrapper.  The
controller starts real app-server turns, drives discovery and pairing through
model tool calls, invokes the target's public approval CLI, and verifies both
relationship stores with separate real model turns.  All cross-process state
is bounded JSON guarded by flock and atomic replacement.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import queue
import signal
import socket as socket_module
import subprocess
import sys
import threading
import time
from typing import Any

from session_script_sdk import RpcError, SessionScriptClient


SOURCE_PAIR_MARKER = "REMOTE_AGENT_COORDINATOR_PAIR_SOURCE"
SOURCE_LIST_MARKER = "REMOTE_AGENT_COORDINATOR_LIST_SOURCE"
TARGET_LIST_MARKER = "REMOTE_AGENT_COORDINATOR_LIST_TARGET"
SOURCE_HOST_ID = "host_source"
TARGET_HOST_ID = "host_target"
MAX_EVENTS = 128
MAX_EVENT_BYTES = 8192
POLL_SECONDS = 0.05
READER_JOIN_TIMEOUT_SECONDS = 1.0
_STATE_LOCK = threading.RLock()


def _json(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


class Recorder:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.count = 0
        self.lock = threading.Lock()

    def add(self, event: str, **fields: object) -> None:
        encoded = _json({"event": event, **fields})
        if len(encoded.encode()) > MAX_EVENT_BYTES:
            raise RuntimeError("coordinator pairing E2E evidence event is too large")
        with self.lock:
            if self.count >= MAX_EVENTS:
                raise RuntimeError(
                    "coordinator pairing E2E evidence event limit exceeded"
                )
            with self.path.open("a", encoding="utf-8") as output:
                output.write(encoded + "\n")
            self.count += 1


@contextmanager
def _state_lock(path: Path, exclusive: bool) -> Any:
    lock_path = path.with_suffix(path.suffix + ".lock")
    with _STATE_LOCK, lock_path.open("a+", encoding="utf-8") as lock_file:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH)
        try:
            yield
        finally:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)


def _state_read_unlocked(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(f"invalid coordinator pairing E2E state: {path}") from error
    if not isinstance(value, dict):
        raise RuntimeError("coordinator pairing E2E state must be a JSON object")
    return value


def _state_read(path: Path) -> dict[str, Any]:
    with _state_lock(path, exclusive=False):
        return _state_read_unlocked(path)


def _state_update(path: Path, **updates: object) -> None:
    with _state_lock(path, exclusive=True):
        state = _state_read_unlocked(path)
        state.update(updates)
        temporary = path.with_name(
            f".{path.name}.{os.getpid()}.{threading.get_ident()}.tmp"
        )
        try:
            temporary.write_text(_json(state) + "\n", encoding="utf-8")
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def _usage() -> dict[str, object]:
    return {
        "input_tokens": 1,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": 1,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": 2,
    }


def _event(kind: str, **fields: object) -> dict[str, object]:
    return {"type": kind, **fields}


def _completed(response_id: str) -> dict[str, object]:
    return _event("response.completed", response={"id": response_id, "usage": _usage()})


def _function_call(
    call_id: str, name: str, arguments: dict[str, object]
) -> list[dict[str, object]]:
    if not name.startswith("remote_") or name == "remote_":
        raise ValueError(f"remote tool name must start with 'remote_': {name!r}")
    wire_name = name.removeprefix("remote_")
    response_id = f"coordinator-pairing-{call_id}"
    return [
        _event("response.created", response={"id": response_id}),
        _event(
            "response.output_item.done",
            item={
                "type": "function_call",
                "call_id": call_id,
                "name": wire_name,
                "namespace": "remote",
                "arguments": _json(arguments),
            },
        ),
        _completed(response_id),
    ]


def _assistant(text: str) -> list[dict[str, object]]:
    response_id = "coordinator-pairing-final"
    return [
        _event("response.created", response={"id": response_id}),
        _event(
            "response.output_item.done",
            item={
                "type": "message",
                "role": "assistant",
                "id": f"{response_id}-message",
                "status": "completed",
                "content": [{"type": "output_text", "text": text}],
            },
        ),
        _completed(response_id),
    ]


def _decode_json(value: Any) -> Any | None:
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return None
    if isinstance(value, dict):
        return value
    if isinstance(value, list):
        for item in value:
            if isinstance(item, dict):
                decoded = _decode_json(item.get("text") or item.get("content"))
                if decoded is not None:
                    return decoded
    return None


def _call_outputs(request: dict[str, Any]) -> dict[str, Any]:
    outputs: dict[str, Any] = {}
    for item in request.get("input", []):
        if not isinstance(item, dict) or item.get("type") not in {
            "function_call_output",
            "custom_tool_call_output",
        }:
            continue
        call_id = item.get("call_id")
        if not isinstance(call_id, str):
            continue
        decoded = _decode_json(item.get("output"))
        if decoded is None:
            decoded = _decode_json(item.get("content"))
        outputs[call_id] = decoded if decoded is not None else item
    return outputs


def _find_candidate(value: Any) -> dict[str, Any] | None:
    if isinstance(value, dict):
        if value.get("hostId") == TARGET_HOST_ID and value.get("role") == "coordinator":
            fingerprint = value.get("fingerprint")
            if isinstance(fingerprint, str) and fingerprint:
                return value
        for child in value.values():
            found = _find_candidate(child)
            if found is not None:
                return found
    if isinstance(value, list):
        for child in value:
            found = _find_candidate(child)
            if found is not None:
                return found
    return None


def _find_host_status(value: Any, host_id: str) -> str | None:
    if isinstance(value, dict):
        if value.get("hostId") == host_id and isinstance(value.get("status"), str):
            return value["status"]
        for child in value.values():
            status = _find_host_status(child, host_id)
            if status is not None:
                return status
    if isinstance(value, list):
        for child in value:
            status = _find_host_status(child, host_id)
            if status is not None:
                return status
    return None


def _source_pair_events(
    request: dict[str, Any], state_file: Path
) -> tuple[str, list[dict[str, object]]]:
    outputs = _call_outputs(request)
    encoded = _json(request)
    if SOURCE_PAIR_MARKER not in encoded:
        raise RuntimeError("source pairing mock did not receive its marker")
    if not outputs:
        return "discover", _function_call(
            "discover", "remote_hosts_discover", {"timeoutSeconds": 3}
        )
    if "pair" in outputs:
        result = outputs["pair"]
        if not isinstance(result, dict) or result.get("status") != "ok":
            raise RuntimeError("coordinator pair did not return ok")
        pending = result.get("result")
        if not isinstance(pending, dict) or pending.get("status") != "pending":
            raise RuntimeError("coordinator pair did not return pending")
        _state_update(state_file, sourcePairPending=pending)
        return "pair-final", _assistant(
            "coordinator pairing request is pending target-owner approval"
        )
    if "discover" in outputs:
        candidate = _find_candidate(outputs["discover"])
        if candidate is None:
            raise RuntimeError(
                "discovery omitted unpaired coordinator host_target with fingerprint"
            )
        fingerprint = candidate["fingerprint"]
        _state_update(
            state_file, discoveredCandidate=candidate, targetFingerprint=fingerprint
        )
        return "pair", _function_call(
            "pair",
            "remote_host_pair",
            {
                "hostId": TARGET_HOST_ID,
                "role": "coordinator",
                "fingerprint": fingerprint,
            },
        )
    raise RuntimeError(
        "source pairing Responses request did not match the state machine"
    )


def _list_events(
    request: dict[str, Any], marker: str, expected_host: str
) -> tuple[str, list[dict[str, object]]]:
    outputs = _call_outputs(request)
    if marker not in _json(request):
        raise RuntimeError("verification mock did not receive its marker")
    if not outputs:
        return "list", _function_call("list", "remote_hosts_list", {})
    if "list" not in outputs:
        raise RuntimeError("verification turn omitted remote_hosts_list output")
    if _find_host_status(outputs["list"], expected_host) != "paired":
        raise RuntimeError(f"remote_hosts_list did not report {expected_host} paired")
    return "list-final", _assistant(f"{expected_host} is paired")


class ResponsesHandler(BaseHTTPRequestHandler):
    role: str
    request_log: Recorder
    state_file: Path

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        try:
            length = int(self.headers["content-length"])
            request = json.loads(self.rfile.read(length))
            if not isinstance(request, dict):
                raise ValueError
        except (KeyError, UnicodeDecodeError, ValueError, json.JSONDecodeError):
            self.send_error(400)
            return
        try:
            encoded = _json(request)
            if self.role == "source" and SOURCE_LIST_MARKER in encoded:
                step, events = _list_events(request, SOURCE_LIST_MARKER, TARGET_HOST_ID)
            elif self.role == "source" and SOURCE_PAIR_MARKER in encoded:
                step, events = _source_pair_events(request, self.state_file)
            elif self.role == "target" and TARGET_LIST_MARKER in encoded:
                step, events = _list_events(request, TARGET_LIST_MARKER, SOURCE_HOST_ID)
            else:
                raise RuntimeError(f"unexpected {self.role} Responses request")
        except RuntimeError as error:
            self.request_log.add("mockError", role=self.role, error=str(error))
            self.send_error(503, str(error))
            return
        self.request_log.add(
            "responses",
            role=self.role,
            step=step,
            callIds=sorted(_call_outputs(request)),
        )
        self._send_events(events)

    def _send_events(self, events: list[dict[str, object]]) -> None:
        body = b"".join(
            f"event: {event['type']}\ndata: {_json(event)}\n\n".encode()
            for event in events
        )
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except BrokenPipeError:
            pass

    def log_message(self, format: str, *args: object) -> None:
        del format, args


class Controller:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.recorder = Recorder(args.evidence_file)
        self.source = self._connect_client("source", args.source_socket)
        self.source_list: SessionScriptClient | None = None
        self.target = self._connect_client("target", args.target_socket)
        self.completed: dict[str, set[str]] = {"source": set(), "target": set()}
        self.reader_errors: queue.Queue[tuple[str, BaseException]] = queue.Queue(
            maxsize=3
        )
        self.reader_threads: list[threading.Thread] = []
        self.closing = threading.Event()

    def _connect_client(self, role: str, socket: Path) -> SessionScriptClient:
        client = SessionScriptClient.connect_unix_socket(socket, self.args.timeout)
        client.set_notification_handler(
            lambda message: self._notification(role, message)
        )
        client.set_server_request_handler(
            lambda message: self._server_request(role, message)
        )
        return client

    def _server_request(self, role: str, message: dict[str, Any]) -> dict[str, Any]:
        if message.get("method") != "item/extensionInteraction/request":
            raise RuntimeError(
                f"unexpected {role} app-server request: {message.get('method')!r}"
            )
        params = message.get("params")
        if not isinstance(params, dict):
            raise RuntimeError("extension interaction omitted params")
        surface = params.get("surface")
        actions = surface.get("actions") if isinstance(surface, dict) else None
        if not isinstance(actions, list):
            raise RuntimeError("extension interaction omitted actions")
        action = next(
            (
                item
                for item in actions
                if isinstance(item, dict)
                and item.get("id") in {"approve", "accept", "approve-session"}
            ),
            None,
        )
        if action is None:
            raise RuntimeError("extension interaction has no normal approve action")
        self.recorder.add(
            "extensionInteractionAccepted", role=role, actionId=action["id"]
        )
        return {
            "extensionId": params.get("extensionId"),
            "interactionId": params.get("interactionId"),
            "continuation": params.get("continuation"),
            "stateRevision": params.get("stateRevision"),
            "outcome": "accepted",
            "action": {"id": action["id"]},
            "values": {},
        }

    def _notification(self, role: str, message: dict[str, Any]) -> None:
        if message.get("method") != "turn/completed":
            return
        params = message.get("params")
        turn = params.get("turn") if isinstance(params, dict) else None
        if not isinstance(turn, dict) or not isinstance(turn.get("id"), str):
            raise RuntimeError("turn/completed omitted turn id")
        status = turn.get("status")
        self.recorder.add(
            "turnCompleted",
            role=role,
            threadId=turn.get("threadId"),
            turnId=turn["id"],
            status=status,
        )
        if status == "completed":
            self.completed[role].add(turn["id"])

    def _start_turn(
        self, client: SessionScriptClient, cwd: str, marker: str
    ) -> tuple[str, str]:
        response = client.request("thread/start", {"cwd": cwd})
        thread = response.get("thread")
        thread_id = thread.get("id") if isinstance(thread, dict) else None
        if not isinstance(thread_id, str):
            raise RuntimeError("thread/start omitted thread id")
        result = client.request(
            "turn/start",
            {"threadId": thread_id, "input": [{"type": "text", "text": marker}]},
        )
        turn = result.get("turn")
        turn_id = turn.get("id") if isinstance(turn, dict) else None
        if not isinstance(turn_id, str):
            raise RuntimeError("turn/start omitted turn id")
        return thread_id, turn_id

    def _wait_turn(self, role: str, turn_id: str) -> None:
        deadline = time.monotonic() + self.args.wait_timeout
        while time.monotonic() < deadline:
            if turn_id in self.completed[role]:
                self._raise_reader_error()
                return
            self._wait_for_reader_activity()
        raise RuntimeError(f"timed out waiting for {role} turn {turn_id}")

    def _wait_pending(self) -> dict[str, Any]:
        deadline = time.monotonic() + self.args.wait_timeout
        while time.monotonic() < deadline:
            pending = _state_read(self.args.state_file).get("sourcePairPending")
            if isinstance(pending, dict):
                return pending
            self._wait_for_reader_activity()
        raise RuntimeError(
            "timed out waiting for source coordinator pairing pending state"
        )

    def _approve_target(self) -> dict[str, Any]:
        env = os.environ.copy()
        env["HOME"] = str(self.args.target_home)
        env["XEDOC_HOME"] = str(self.args.target_home)
        for item in self.args.target_env:
            key, separator, value = item.partition("=")
            if not separator or not key:
                raise RuntimeError(f"invalid --target-env value: {item!r}")
            env[key] = value
        completed = subprocess.run(
            [
                str(self.args.target_session_command),
                "remote-agent-pairings",
                "--approve",
                SOURCE_HOST_ID,
            ],
            cwd=self.args.source_cwd,
            env=env,
            text=True,
            capture_output=True,
            timeout=self.args.command_timeout,
            check=False,
        )
        if completed.returncode != 0:
            raise RuntimeError(
                f"target pairing approval CLI failed ({completed.returncode}): {completed.stderr.strip()[:512]}"
            )
        try:
            result = json.loads(completed.stdout)
        except json.JSONDecodeError as error:
            raise RuntimeError(
                "target pairing approval CLI did not emit JSON"
            ) from error
        if not isinstance(result, dict) or result.get("status") != "paired":
            raise RuntimeError("target pairing approval CLI did not return paired")
        return result

    def run(self) -> None:
        self.source.initialize(
            "coordinator-pair-source", "Coordinator pairing source", "0.1.0"
        )
        self.target.initialize(
            "coordinator-pair-target", "Coordinator pairing target", "0.1.0"
        )
        source_thread, source_turn = self._start_turn(
            self.source, self.args.source_cwd, SOURCE_PAIR_MARKER
        )
        self._start_reader("source", self.source)
        self.args.ready_file.write_text(
            _json({"sourceThreadId": source_thread, "sourceTurnId": source_turn})
            + "\n",
            encoding="utf-8",
        )
        self.recorder.add(
            "sourcePairTurnStarted", threadId=source_thread, turnId=source_turn
        )
        pending = self._wait_pending()
        discovered = _state_read(self.args.state_file).get("discoveredCandidate")
        if not isinstance(discovered, dict):
            raise RuntimeError(
                "source pairing did not persist its discovered candidate"
            )
        self.recorder.add("discoveredCandidate", candidate=discovered)
        self.recorder.add("pendingPair", result=pending)
        approval = self._approve_target()
        self.recorder.add("targetCliApproval", result=approval)
        self._wait_turn("source", source_turn)
        self.source_list = self._connect_client("source", self.args.source_socket)
        self.source_list.initialize(
            "coordinator-pair-source-list", "Coordinator pairing source list", "0.1.0"
        )
        source_list_thread, source_list_turn = self._start_turn(
            self.source_list, self.args.source_cwd, SOURCE_LIST_MARKER
        )
        self._start_reader("source", self.source_list)
        self._wait_turn("source", source_list_turn)
        self.recorder.add(
            "sourcePairedList",
            threadId=source_list_thread,
            turnId=source_list_turn,
            hostId=TARGET_HOST_ID,
            status="paired",
        )
        target_list_thread, target_list_turn = self._start_turn(
            self.target, self.args.source_cwd, TARGET_LIST_MARKER
        )
        self._start_reader("target", self.target)
        self._wait_turn("target", target_list_turn)
        self.recorder.add(
            "targetPairedList",
            threadId=target_list_thread,
            turnId=target_list_turn,
            hostId=SOURCE_HOST_ID,
            status="paired",
        )
        self.recorder.add(
            "controllerPassed",
            discoveredCandidate=discovered,
            sourceTurn=source_turn,
            targetTurn=target_list_turn,
        )

    def _start_reader(self, role: str, client: SessionScriptClient) -> None:
        client._transport.socket.settimeout(None)
        reader = threading.Thread(
            target=self._read_messages,
            args=(role, client),
            daemon=True,
            name=f"coordinator-pairing-e2e-{role}-reader",
        )
        self.reader_threads.append(reader)
        reader.start()

    def _read_messages(self, role: str, client: SessionScriptClient) -> None:
        try:
            while not self.closing.is_set():
                message = client.receive_message()
                if message.get("method") is None:
                    raise RuntimeError(
                        f"unexpected {role} controller response after reader startup"
                    )
                client.handle_message(message)
        except BaseException as error:
            if not self.closing.is_set():
                self.reader_errors.put_nowait((role, error))

    def _wait_for_reader_activity(self) -> None:
        self._raise_reader_error()
        time.sleep(POLL_SECONDS)
        self._raise_reader_error()

    def _raise_reader_error(self) -> None:
        try:
            role, error = self.reader_errors.get_nowait()
        except queue.Empty:
            return
        raise RuntimeError(f"{role} controller reader failed: {error}") from error

    def close(self) -> None:
        self.closing.set()
        clients = [self.source, self.target]
        if self.source_list is not None:
            clients.append(self.source_list)
        for client in clients:
            try:
                client._transport.socket.shutdown(socket_module.SHUT_RDWR)
            except OSError:
                pass
            client.close()
        for reader in self.reader_threads:
            reader.join(READER_JOIN_TIMEOUT_SECONDS)
            if reader.is_alive():
                raise RuntimeError(
                    f"controller reader {reader.name!r} did not stop after "
                    f"{READER_JOIN_TIMEOUT_SECONDS:.1f}s"
                )


def _run_mock(args: argparse.Namespace) -> int:
    args.request_log.touch()
    handler = type("CoordinatorPairingResponsesHandler", (ResponsesHandler,), {})
    handler.role = args.role
    handler.request_log = Recorder(args.request_log)
    handler.state_file = args.state_file
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    args.port_file.write_text(str(server.server_port), encoding="utf-8")

    def stop(_: int, __: Any) -> None:
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    server.serve_forever()
    server.server_close()
    return 0


def _run_controller(args: argparse.Namespace) -> int:
    controller = Controller(args)
    try:
        controller.run()
    finally:
        controller.close()
    return 0


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    mock = commands.add_parser(
        "mock", help="serve one local deterministic Responses mock"
    )
    mock.add_argument("--role", choices=("source", "target"), required=True)
    mock.add_argument("--port-file", type=Path, required=True)
    mock.add_argument("--request-log", type=Path, required=True)
    mock.add_argument("--state-file", type=Path, required=True)
    mock.set_defaults(run=_run_mock)
    controller = commands.add_parser(
        "controller", help="drive coordinator pairing and verify both stores"
    )
    controller.add_argument("--source-socket", type=Path, required=True)
    controller.add_argument("--target-socket", type=Path, required=True)
    controller.add_argument("--source-cwd", required=True)
    controller.add_argument("--target-session-command", type=Path, required=True)
    controller.add_argument("--target-home", type=Path, required=True)
    controller.add_argument(
        "--target-env", action="append", default=[], metavar="KEY=VALUE"
    )
    controller.add_argument("--ready-file", type=Path, required=True)
    controller.add_argument("--evidence-file", type=Path, required=True)
    controller.add_argument("--state-file", type=Path, required=True)
    controller.add_argument("--timeout", type=float, default=0.25)
    controller.add_argument("--wait-timeout", type=float, default=90.0)
    controller.add_argument("--command-timeout", type=float, default=30.0)
    controller.set_defaults(run=_run_controller)
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        return args.run(args)
    except (
        OSError,
        RpcError,
        RuntimeError,
        ValueError,
        subprocess.TimeoutExpired,
    ) as error:
        print(f"coordinator pairing E2E failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
