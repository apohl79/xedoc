#!/usr/bin/env python3
"""Deterministic Responses API mock for the session-script tmux E2E."""

import argparse
import json
import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import signal
import threading
from typing import Any


PROMPT_MARKER = "SESSION_SCRIPT_E2E_PROMPT"
STEER_MARKER = "SESSION_SCRIPT_E2E_STEER"
PRIMARY_INPUT_CALL_ID = "session-script-input-primary"
FALLBACK_INPUT_CALL_ID = "session-script-input-fallback"
PERMISSIONS_CALL_ID = "session-script-permissions"
PATCH_CALL_ID = "session-script-patch"
COMMAND_CALL_ID = "session-script-command"


def event(kind: str, **fields: object) -> dict[str, object]:
    return {"type": kind, **fields}


def completed(response_id: str) -> dict[str, object]:
    return event(
        "response.completed",
        response={
            "id": response_id,
            "usage": {
                "input_tokens": 1,
                "input_tokens_details": {"cached_tokens": 0},
                "output_tokens": 1,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 2,
            },
        },
    )


def function_call(
    response_id: str, call_id: str, name: str, arguments: dict[str, object]
) -> list[dict[str, object]]:
    return [
        event("response.created", response={"id": response_id}),
        event(
            "response.output_item.done",
            item={
                "type": "function_call",
                "call_id": call_id,
                "name": name,
                "arguments": json.dumps(arguments, separators=(",", ":")),
            },
        ),
        completed(response_id),
    ]


def request_user_input_response(
    call_id: str, question_id: str, auto_resolution_ms: int
) -> list[dict[str, object]]:
    arguments = {
        "questions": [
            {
                "id": question_id,
                "header": "Confirm",
                "question": "Continue?",
                "options": [
                    {"label": "Yes", "description": "Continue."},
                    {"label": "No", "description": "Stop."},
                ],
            }
        ],
        "autoResolutionMs": auto_resolution_ms,
    }
    return function_call(
        f"response-{call_id}", call_id, "request_user_input", arguments
    )


def permissions_response() -> list[dict[str, object]]:
    return function_call(
        "response-session-script-permissions",
        PERMISSIONS_CALL_ID,
        "request_permissions",
        {
            "reason": "Exercise the permissions observation path.",
            "permissions": {"file_system": {"write": ["."]}},
        },
    )


def patch_response() -> list[dict[str, object]]:
    return [
        event("response.created", response={"id": "response-session-script-patch"}),
        event(
            "response.output_item.done",
            item={
                "type": "custom_tool_call",
                "call_id": PATCH_CALL_ID,
                "name": "apply_patch",
                "input": (
                    "*** Begin Patch\n"
                    "*** Add File: session-script-e2e-should-not-exist.txt\n"
                    "+declined\n"
                    "*** End Patch\n"
                ),
            },
        ),
        completed("response-session-script-patch"),
    ]


def command_response() -> list[dict[str, object]]:
    return function_call(
        "response-session-script-command",
        COMMAND_CALL_ID,
        "shell_command",
        {
            "command": "printf session-script-command",
            "sandbox_permissions": "require_escalated",
            "justification": "Exercise the command approval observation path.",
        },
    )


def final_response() -> list[dict[str, object]]:
    return [
        event("response.created", response={"id": "session-script-final"}),
        event(
            "response.output_item.added",
            item={
                "type": "message",
                "role": "assistant",
                "id": "session-script-message",
                "status": "in_progress",
                "content": [],
            },
        ),
        event("response.output_text.delta", delta="session-script partial "),
        event("response.output_text.delta", delta="answer"),
        event(
            "response.output_text.done",
            item_id="session-script-message",
            output_index=0,
            content_index=0,
            text="session-script answer",
        ),
        event(
            "response.content_part.done",
            item_id="session-script-message",
            output_index=0,
            content_index=0,
            part={
                "type": "output_text",
                "text": "session-script answer",
                "annotations": [],
            },
        ),
        event(
            "response.output_item.done",
            item={
                "type": "message",
                "role": "assistant",
                "id": "session-script-message",
                "status": "completed",
                "content": [{"type": "output_text", "text": "session-script answer"}],
            },
        ),
        completed("session-script-final"),
    ]


def call_output_ids(request: dict[str, Any]) -> set[str]:
    request_input = request.get("input")
    if not isinstance(request_input, list):
        return set()
    return {
        item["call_id"]
        for item in request_input
        if isinstance(item, dict)
        and item.get("type") in ("function_call_output", "custom_tool_call_output")
        and isinstance(item.get("call_id"), str)
    }


def response_for_request(request: dict[str, Any]) -> list[dict[str, object]]:
    outputs = call_output_ids(request)
    if PRIMARY_INPUT_CALL_ID not in outputs:
        return request_user_input_response(
            PRIMARY_INPUT_CALL_ID, "confirm_path", 60_000
        )
    if FALLBACK_INPUT_CALL_ID not in outputs:
        return request_user_input_response(FALLBACK_INPUT_CALL_ID, "fallback_path", 100)
    if PERMISSIONS_CALL_ID not in outputs:
        return permissions_response()
    if PATCH_CALL_ID not in outputs:
        return patch_response()
    if COMMAND_CALL_ID not in outputs:
        return command_response()
    return final_response()


class Handler(BaseHTTPRequestHandler):
    port_file: Path
    request_log: Path
    request_log_lock = threading.Lock()

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        try:
            request = json.loads(self.rfile.read(int(self.headers["content-length"])))
        except (KeyError, json.JSONDecodeError, ValueError):
            self.send_error(400)
            return
        encoded = json.dumps(request, separators=(",", ":"))
        observation = json.dumps(
            {
                "callOutputIds": sorted(call_output_ids(request)),
                "hasPromptMarker": PROMPT_MARKER in encoded,
                "hasSteerMarker": STEER_MARKER in encoded,
            },
            separators=(",", ":"),
        )
        with self.request_log_lock:
            fd = os.open(
                self.request_log, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600
            )
            try:
                os.write(fd, (observation + "\n").encode())
            finally:
                os.close(fd)
        events = response_for_request(request)
        body = b"".join(
            f"event: {item['type']}\ndata: {json.dumps(item, separators=(',', ':'))}\n\n".encode()
            for item in events
        )
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        del format, args


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", type=Path, required=True)
    parser.add_argument("--request-log", type=Path, required=True)
    args = parser.parse_args()
    args.request_log.touch()
    Handler.port_file = args.port_file
    Handler.request_log = args.request_log
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    args.port_file.write_text(str(server.server_port), encoding="utf-8")

    def stop(_: int, __: Any) -> None:
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)
    server.serve_forever()
    server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
