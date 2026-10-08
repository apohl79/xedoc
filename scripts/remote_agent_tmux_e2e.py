#!/usr/bin/env python3
"""Small deterministic controller and Responses mock for remote-agent E2E."""

from __future__ import annotations

import argparse
import json
import os
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
from pathlib import Path
import signal
import socket
import sys
import threading
import time
from typing import Any

from session_script_sdk import RpcError
from session_script_sdk import SessionScriptClient


SOURCE_MARKER = "REMOTE_AGENT_E2E_SOURCE_START"
PAYLOAD_MARKER = "REMOTE_AGENT_E2E_PAYLOAD"
APPROVAL_MARKER = "REMOTE_AGENT_E2E_APPROVAL"
CORRELATION_ID = "corr_remote_e2e"
PAIR_CALL_ID = "remote-e2e-pair"
GRANT_CALL_ID = "remote-e2e-grant"
SESSION_SEND_CALL_ID = "remote-e2e-session-send"
MESSAGE_CALL_ID = "remote-e2e-message"
REVIEW_APPROVE_CALL_ID = "remote-e2e-review-approve"
APPROVE_CALL_ID = "remote-e2e-approve"
REVIEW_REJECT_CALL_ID = "remote-e2e-review-reject"
REJECT_CALL_ID = "remote-e2e-reject"
TARGET_EXEC_CALL_ID = "remote-e2e-target-exec"
MAX_LOG_EVENTS = 256
MAX_LOG_BYTES = 8 * 1024


def _json(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


class Recorder:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.count = 0

    def add(self, event: str, **fields: object) -> None:
        if self.count >= MAX_LOG_EVENTS:
            raise RuntimeError("E2E event log limit exceeded")
        encoded = _json({"event": event, **fields})
        if len(encoded.encode("utf-8")) > MAX_LOG_BYTES:
            raise RuntimeError("E2E event exceeds its size limit")
        with self.path.open("a", encoding="utf-8") as output:
            output.write(encoded + "\n")
        self.count += 1


def _thread_id(response: dict[str, Any]) -> str:
    thread = response.get("thread")
    if not isinstance(thread, dict) or not isinstance(thread.get("id"), str):
        raise RuntimeError("thread/start did not return a thread id")
    return thread["id"]


def _selected_approval(params: dict[str, Any]) -> str:
    surface = params.get("surface")
    if not isinstance(surface, dict):
        raise RuntimeError("remote approval has no surface")
    actions = surface.get("actions")
    if not isinstance(actions, list):
        raise RuntimeError("remote approval surface has no actions")
    if any(
        isinstance(action, dict) and action.get("id") == "approve-session"
        for action in actions
    ):
        return "approve-session"
    raise RuntimeError("remote approval surface has no approve-session action")


class Controller:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.recorder = Recorder(args.log)
        self.client = SessionScriptClient.connect_unix_socket(args.socket, args.timeout)
        self.client.set_notification_handler(self._notification)
        self.client.set_server_request_handler(self._server_request)
        self.approvals = 0
        self.saw_target_transcript = False
        self.saw_target_command_decline = False
        self.turn_completed = False

    def _server_request(self, message: dict[str, Any]) -> dict[str, Any]:
        method = message.get("method")
        params = message.get("params")
        if not isinstance(params, dict):
            raise RuntimeError(f"unexpected app-server request: {method!r}")
        if method == "item/commandExecution/requestApproval":
            if self.args.role != "target":
                raise RuntimeError("only the target owner may resolve command approval")
            self.saw_target_command_decline = True
            self.recorder.add(
                "targetCommandApprovalDeclined",
                threadId=params.get("threadId"),
            )
            return {"decision": "decline"}
        if method != "item/extensionInteraction/request":
            raise RuntimeError(f"unexpected app-server request: {method!r}")
        action_id = _selected_approval(params)
        self.approvals += 1
        self.recorder.add(
            "remoteApproval",
            actionId=action_id,
            extensionId=params.get("extensionId"),
        )
        return {
            "extensionId": params.get("extensionId"),
            "interactionId": params.get("interactionId"),
            "continuation": params.get("continuation"),
            "stateRevision": params.get("stateRevision"),
            "outcome": "accepted",
            "action": {"id": action_id},
            "values": {},
        }

    def _notification(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        params = message.get("params")
        if not isinstance(method, str) or not isinstance(params, dict):
            return
        if method == "item/completed":
            encoded = _json(params)
            markers = {
                "payload": PAYLOAD_MARKER in encoded,
                "originHost": "Origin host: host_coord" in encoded,
                "originThread": "Origin thread:" in encoded,
                "correlation": f"Correlation: {CORRELATION_ID}" in encoded,
            }
            if all(markers.values()):
                self.saw_target_transcript = True
            self.recorder.add("itemCompleted", **markers)
        elif method == "turn/completed":
            self.turn_completed = True
            turn = params.get("turn")
            self.recorder.add(
                "turnCompleted",
                status=turn.get("status") if isinstance(turn, dict) else None,
            )

    def run(self) -> None:
        self.client.initialize(
            "remote-agent-e2e-controller",
            "Remote agent E2E controller",
            "0.1.0",
        )
        thread_id = _thread_id(
            self.client.request("thread/start", {"cwd": self.args.cwd})
        )
        self.args.ready_file.write_text(
            _json({"threadId": thread_id}) + "\n", encoding="utf-8"
        )
        self.recorder.add("threadReady", threadId=thread_id, role=self.args.role)
        if self.args.role == "source":
            self.client.request(
                "turn/start",
                {
                    "threadId": thread_id,
                    "input": [{"type": "text", "text": SOURCE_MARKER}],
                },
            )
            self.recorder.add("sourceTurnStarted", threadId=thread_id)
        self._wait()
        if self.args.role == "source":
            if self.approvals != 8:
                raise RuntimeError(
                    f"expected eight local owner approvals, got {self.approvals}"
                )
            if not self.turn_completed:
                raise RuntimeError("source turn did not complete")
        elif not self.saw_target_transcript:
            raise RuntimeError("target did not receive the provenance transcript")
        elif not self.saw_target_command_decline:
            raise RuntimeError("target owner did not decline the command approval")
        elif not self.turn_completed:
            raise RuntimeError("target delivery turn did not complete")
        self.recorder.add(
            "controllerPassed",
            approvals=self.approvals,
            targetTranscript=self.saw_target_transcript,
        )

    def _wait(self) -> None:
        deadline = time.monotonic() + self.args.wait_timeout
        while time.monotonic() < deadline:
            if self.args.role == "source" and self.turn_completed:
                return
            if (
                self.args.role == "target"
                and self.saw_target_transcript
                and self.turn_completed
            ):
                return
            try:
                self.client.handle_message(self.client.receive_message())
            except socket.timeout:
                continue
        raise RuntimeError("timed out waiting for remote-agent E2E completion")

    def close(self) -> None:
        self.client.close()


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
    response_id: str,
    call_id: str,
    name: str,
    arguments: dict[str, object],
    namespace: str | None = "remote",
) -> list[dict[str, object]]:
    item: dict[str, object] = {
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": _json(arguments),
    }
    if namespace is not None:
        item["namespace"] = namespace
    return [
        _event("response.created", response={"id": response_id}),
        _event(
            "response.output_item.done",
            item=item,
        ),
        _completed(response_id),
    ]


def _assistant(response_id: str, text: str) -> list[dict[str, object]]:
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


def _call_output_ids(request: dict[str, Any]) -> set[str]:
    value = request.get("input")
    if not isinstance(value, list):
        return set()
    return {
        item["call_id"]
        for item in value
        if isinstance(item, dict)
        and item.get("type") in {"function_call_output", "custom_tool_call_output"}
        and isinstance(item.get("call_id"), str)
    }


def _target_thread(path: Path) -> str:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError("target thread is unavailable") from error
    thread_id = value.get("threadId") if isinstance(value, dict) else None
    if not isinstance(thread_id, str) or not thread_id:
        raise RuntimeError("target thread id is unavailable")
    return thread_id


def _remote_tool_names(request: dict[str, Any]) -> list[str]:
    names: list[str] = []

    def visit(value: object, in_remote_namespace: bool = False) -> None:
        if isinstance(value, list):
            for item in value:
                visit(item, in_remote_namespace)
        elif isinstance(value, dict):
            namespace = value.get("namespace")
            name = value.get("name")
            remote_namespace = in_remote_namespace or namespace == "remote"
            if remote_namespace and isinstance(name, str):
                names.append(name)
            if value.get("type") == "namespace" and name == "remote":
                remote_namespace = True
            for item in value.values():
                visit(item, remote_namespace)

    visit(request.get("tools"))
    return sorted(names)


def _events_for_request(
    request: dict[str, Any], target_thread_file: Path
) -> tuple[str, list[dict[str, object]]]:
    outputs = _call_output_ids(request)
    encoded = _json(request)
    if TARGET_EXEC_CALL_ID in outputs:
        return "targetFinal", _assistant(
            "remote-e2e-target-final", "remote-agent E2E target complete"
        )
    if MESSAGE_CALL_ID in outputs:
        return "sourceFinal", _assistant(
            "remote-e2e-source-final", "remote-agent E2E source complete"
        )
    if SESSION_SEND_CALL_ID in outputs:
        return "message", _function_call(
            "remote-e2e-message-response",
            MESSAGE_CALL_ID,
            "session_message",
            {
                "messageId": "msg_remote_e2e",
                "correlationId": CORRELATION_ID,
                "target": {
                    "hostId": "host_managed",
                    "threadId": _target_thread(target_thread_file),
                },
                "body": PAYLOAD_MARKER,
                "delivery": "defer",
            },
        )
    if GRANT_CALL_ID in outputs:
        return "sessionSend", _function_call(
            "remote-e2e-session-send-response",
            SESSION_SEND_CALL_ID,
            "session_send",
            {
                "hostId": "host_managed",
                "threadId": _target_thread(target_thread_file),
                "message": APPROVAL_MARKER,
            },
        )
    if PAIR_CALL_ID in outputs:
        return "grant", _function_call(
            "remote-e2e-grant-response",
            GRANT_CALL_ID,
            "host_grant_set",
            {
                "hostId": "host_managed",
                "scopes": ["sessionWrite"],
                "expiresAt": int(time.time()) + 3600,
            },
        )
    if REJECT_CALL_ID in outputs:
        return "pair", _function_call(
            "remote-e2e-pair-response",
            PAIR_CALL_ID,
            "host_pair",
            {"hostId": "host_managed", "role": "managed"},
        )
    if REVIEW_REJECT_CALL_ID in outputs:
        return "reject", _function_call(
            "remote-e2e-reject-response",
            REJECT_CALL_ID,
            "request_reject",
            {"requestId": "review-rejected"},
        )
    if APPROVE_CALL_ID in outputs:
        return "reviewReject", _function_call(
            "remote-e2e-review-reject-response",
            REVIEW_REJECT_CALL_ID,
            "request_review",
            {"requestId": "review-rejected"},
        )
    if REVIEW_APPROVE_CALL_ID in outputs:
        return "approve", _function_call(
            "remote-e2e-approve-response",
            APPROVE_CALL_ID,
            "request_approve",
            {"requestId": "review-approved"},
        )
    if SOURCE_MARKER in encoded:
        return "reviewApprove", _function_call(
            "remote-e2e-review-approve-response",
            REVIEW_APPROVE_CALL_ID,
            "request_review",
            {"requestId": "review-approved"},
        )
    if APPROVAL_MARKER in encoded:
        return "targetApproval", _function_call(
            "remote-e2e-target-approval-response",
            TARGET_EXEC_CALL_ID,
            "exec_command",
            {"cmd": "rm -f remote-agent-e2e-nonexistent"},
            namespace=None,
        )
    return "targetFinal", _assistant(
        "remote-e2e-target-final", "remote-agent E2E target complete"
    )


class ResponsesHandler(BaseHTTPRequestHandler):
    port_file: Path
    request_log: Path
    target_thread_file: Path
    request_log_lock = threading.Lock()

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        try:
            content_length = int(self.headers["content-length"])
            request = json.loads(self.rfile.read(content_length))
        except (KeyError, UnicodeDecodeError, ValueError, json.JSONDecodeError):
            self.send_error(400)
            return
        if not isinstance(request, dict):
            self.send_error(400)
            return
        try:
            step, events = _events_for_request(request, self.target_thread_file)
        except RuntimeError:
            self.send_error(503)
            return
        observation = _json(
            {
                "step": step,
                "callOutputIds": sorted(_call_output_ids(request)),
                "hasSourceMarker": SOURCE_MARKER in _json(request),
                "remoteToolNames": _remote_tool_names(request),
            }
        )
        with self.request_log_lock:
            fd = os.open(
                self.request_log, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600
            )
            try:
                os.write(fd, (observation + "\n").encode("utf-8"))
            finally:
                os.close(fd)
        body = b"".join(
            (f"event: {item['type']}\ndata: {_json(item)}\n\n").encode("utf-8")
            for item in events
        )
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        del format, args


def _run_mock(args: argparse.Namespace) -> int:
    args.request_log.touch()
    ResponsesHandler.port_file = args.port_file
    ResponsesHandler.request_log = args.request_log
    ResponsesHandler.target_thread_file = args.target_thread_file
    server = ThreadingHTTPServer(("127.0.0.1", 0), ResponsesHandler)
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
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    mock = commands.add_parser("mock")
    mock.add_argument("--port-file", type=Path, required=True)
    mock.add_argument("--request-log", type=Path, required=True)
    mock.add_argument("--target-thread-file", type=Path, required=True)
    mock.set_defaults(run=_run_mock)
    controller = commands.add_parser("controller")
    controller.add_argument("--role", choices=("source", "target"), required=True)
    controller.add_argument("--socket", type=Path, required=True)
    controller.add_argument("--cwd", required=True)
    controller.add_argument("--ready-file", type=Path, required=True)
    controller.add_argument("--log", type=Path, required=True)
    controller.add_argument("--timeout", type=float, default=1.0)
    controller.add_argument("--wait-timeout", type=float, default=90.0)
    controller.set_defaults(run=_run_controller)
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        return args.run(args)
    except (OSError, RpcError, RuntimeError, ValueError) as error:
        print(f"remote-agent E2E support failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
