#!/usr/bin/env python3
"""Deterministic local Responses API mock for the standalone router tmux E2E."""

import argparse
import json
import os
import re
import signal
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
from pathlib import Path
from typing import Any
from typing import Optional


ROOT_SPAWN_MARKER = "ROUTER_E2E_SPAWN_ROOT"
ROOT_SPAWN_PREFIX = ROOT_SPAWN_MARKER + "_"
CHILD_MARKER = "ROUTER_E2E_CHILD"
HOLD_RESPONSE_MARKER = "ROUTER_E2E_HOLD_OPEN"
SPAWN_CALL_ID = "router-e2e-spawn-call"
TEST_MARKER_PATTERN = re.compile(r"ROUTER_(?:E2E|CASE)_[A-Za-z0-9_-]+")


class RequestLog:
    """Serializes request observations used by the black-box assertions."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.lock = threading.Lock()

    def append(self, request: dict[str, Any]) -> None:
        line = (
            json.dumps(request, separators=(",", ":"), sort_keys=True) + "\n"
        ).encode()
        with self.lock:
            fd = os.open(self.path, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
            try:
                os.write(fd, line)
            finally:
                os.close(fd)


def contains_text(value: Any, text: str) -> bool:
    return text in json.dumps(value, separators=(",", ":"))


def has_function_call_output(value: dict[str, Any], call_id: str) -> bool:
    return any(
        item.get("type") == "function_call_output" and item.get("call_id") == call_id
        for item in value.get("input", [])
        if isinstance(item, dict)
    )


def observation_metadata(request: dict[str, Any]) -> dict[str, Optional[str]]:
    metadata = request.get("client_metadata")
    if not isinstance(metadata, dict):
        return {"thread_id": None, "turn_id": None, "thread_source": None}
    turn_source = None
    raw_turn_metadata = metadata.get("x-codex-turn-metadata")
    if isinstance(raw_turn_metadata, str):
        try:
            parsed_turn_metadata = json.loads(raw_turn_metadata)
        except json.JSONDecodeError:
            parsed_turn_metadata = None
        if isinstance(parsed_turn_metadata, dict):
            source = parsed_turn_metadata.get("thread_source")
            if isinstance(source, str):
                turn_source = source
    return {
        "thread_id": metadata.get("thread_id")
        if isinstance(metadata.get("thread_id"), str)
        else None,
        "turn_id": metadata.get("turn_id")
        if isinstance(metadata.get("turn_id"), str)
        else None,
        "thread_source": turn_source,
    }


def request_kind(request: dict[str, Any]) -> Optional[str]:
    metadata = request.get("client_metadata")
    if not isinstance(metadata, dict):
        return None
    raw_turn_metadata = metadata.get("x-codex-turn-metadata")
    if not isinstance(raw_turn_metadata, str):
        return None
    try:
        parsed_turn_metadata = json.loads(raw_turn_metadata)
    except json.JSONDecodeError:
        return None
    value = parsed_turn_metadata.get("request_kind")
    return value if isinstance(value, str) else None


def observation(request: dict[str, Any], sequence: int, path: str) -> dict[str, Any]:
    """Returns the bounded, prompt-free request observation asserted by the E2E."""
    return {
        "sequence": sequence,
        "path": path,
        "model": request.get("model"),
        "reasoning": request.get("reasoning"),
        "request_kind": request_kind(request),
        "client_metadata": observation_metadata(request),
        "markers": sorted(set(TEST_MARKER_PATTERN.findall(json.dumps(request)))),
    }


def response_created(response_id: str) -> dict[str, Any]:
    return {"type": "response.created", "response": {"id": response_id}}


def completed(response_id: str) -> dict[str, Any]:
    return {
        "type": "response.completed",
        "response": {
            "id": response_id,
            "usage": {
                "input_tokens": 12,
                "input_tokens_details": {"cached_tokens": 2},
                "output_tokens": 3,
                "output_tokens_details": {"reasoning_tokens": 0},
                "total_tokens": 15,
            },
        },
    }


def assistant_message(message_id: str, text: str) -> dict[str, Any]:
    return {
        "type": "response.output_item.done",
        "item": {
            "type": "message",
            "role": "assistant",
            "id": message_id,
            "content": [{"type": "output_text", "text": text}],
        },
    }


def spawn_agent_call() -> dict[str, Any]:
    arguments = json.dumps(
        {
            "message": (
                ROOT_SPAWN_MARKER + " " + CHILD_MARKER + " " + HOLD_RESPONSE_MARKER
            ),
            "task_name": "router_e2e_child",
            "fork_turns": "none",
        },
        separators=(",", ":"),
    )
    return {
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": SPAWN_CALL_ID,
            "namespace": "multi_agent_v2",
            "name": "spawn_agent",
            "arguments": arguments,
        },
    }


def events_for_request(request: dict[str, Any], sequence: int) -> list[dict[str, Any]]:
    response_id = f"router-e2e-response-{sequence}"
    if request_kind(request) == "model_router_classifier":
        if contains_text(request, CHILD_MARKER):
            classification = (
                '{"work_type":{"group":"group3","steering":false},'
                '"complexity":"low","risk":"low",'
                '"orchestration":"none",'
                '"orchestration_reason":"No explicit delegation or coordination request was present.",'
                '"confidence":0.8}'
            )
        elif contains_text(request, "ROUTER_E2E_CLASSIFIER_SHADOW"):
            classification = (
                '{"work_type":{"group":"group1","steering":false},'
                '"complexity":"very_high",'
                '"risk":"medium","orchestration":"delegate",'
                '"orchestration_reason":"The task contains one bounded independent subtask to delegate.",'
                '"confidence":0.8}'
            )
        elif contains_text(request, "ROUTER_E2E_CLASSIFIER_FULL"):
            classification = (
                '{"work_type":{"group":"group1","steering":false},'
                '"complexity":"high","risk":"low",'
                '"orchestration":"delegate",'
                '"orchestration_reason":"The task explicitly requests a bounded delegation.",'
                '"confidence":0.8}'
            )
        else:
            classification = (
                '{"work_type":{"group":"group1","steering":false},'
                '"complexity":"high","risk":"low",'
                '"orchestration":"none",'
                '"orchestration_reason":"No explicit delegation or coordination request was present.",'
                '"confidence":0.8}'
            )
        return [
            response_created(response_id),
            assistant_message(
                f"router-e2e-classifier-{sequence}",
                classification,
            ),
            completed(response_id),
        ]
    if contains_text(request, ROOT_SPAWN_PREFIX) and not has_function_call_output(
        request, SPAWN_CALL_ID
    ):
        return [
            response_created(response_id),
            spawn_agent_call(),
            completed(response_id),
        ]
    if has_function_call_output(request, SPAWN_CALL_ID):
        return [
            response_created(response_id),
            assistant_message(
                f"router-e2e-parent-{sequence}", "router parent completed"
            ),
            completed(response_id),
        ]
    if contains_text(request, CHILD_MARKER):
        return [
            response_created(response_id),
            assistant_message(f"router-e2e-child-{sequence}", "router child completed"),
            completed(response_id),
        ]
    return [
        response_created(response_id),
        assistant_message(f"router-e2e-message-{sequence}", "router root completed"),
        completed(response_id),
    ]


class Handler(BaseHTTPRequestHandler):
    request_log: RequestLog
    hold_response_file: Path
    request_sequence = 0
    request_sequence_lock = threading.Lock()

    def do_POST(self) -> None:  # noqa: N802
        if self.path != "/v1/responses":
            self.send_error(404)
            return
        length = int(self.headers.get("content-length", "0"))
        try:
            request = json.loads(self.rfile.read(length))
        except json.JSONDecodeError:
            self.send_error(400, "invalid JSON")
            return
        with self.request_sequence_lock:
            type(self).request_sequence += 1
            sequence = type(self).request_sequence
        self.request_log.append(observation(request, sequence, self.path))
        if contains_text(request, HOLD_RESPONSE_MARKER):
            for _ in range(600):
                if self.hold_response_file.exists():
                    break
                time.sleep(0.05)
            else:
                self.send_error(504, "held response was not released")
                return
        body = "".join(
            f"event: {event['type']}\ndata: {json.dumps(event, separators=(',', ':'))}\n\n"
            for event in events_for_request(request, sequence)
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        del format, args


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", type=Path, required=True)
    parser.add_argument("--request-log", type=Path, required=True)
    parser.add_argument("--hold-response-file", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    args.request_log.touch()
    Handler.request_log = RequestLog(args.request_log)
    Handler.hold_response_file = args.hold_response_file
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    args.port_file.write_text(str(server.server_port), encoding="utf-8")

    def stop_server(_signal: int, _frame: object) -> None:
        threading.Thread(target=server.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, stop_server)
    signal.signal(signal.SIGINT, stop_server)
    server.serve_forever()
    server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
