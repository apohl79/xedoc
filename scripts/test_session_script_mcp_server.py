#!/usr/bin/env python3
"""Minimal stdio MCP server that elicits once during its test tool call."""

import argparse
import json
import os
from pathlib import Path
import sys
from typing import Any


MAX_MESSAGE_BYTES = 1 << 20


class Recorder:
    def __init__(self, path: Path) -> None:
        self.path = path

    def add(self, event: str, **fields: object) -> None:
        encoded = json.dumps(
            {"event": event, **fields}, separators=(",", ":"), sort_keys=True
        )
        fd = os.open(self.path, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
        try:
            os.write(fd, (encoded + "\n").encode())
        finally:
            os.close(fd)


def send(message: dict[str, Any]) -> None:
    encoded = json.dumps(message, separators=(",", ":"))
    if len(encoded.encode()) > MAX_MESSAGE_BYTES:
        raise RuntimeError("MCP fixture response exceeds the size limit")
    print(encoded, flush=True)


def result(request_id: Any, value: dict[str, Any]) -> None:
    send({"jsonrpc": "2.0", "id": request_id, "result": value})


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--log", type=Path, required=True)
    args = parser.parse_args()
    recorder = Recorder(args.log)
    pending_tool_call_id: Any = None
    elicitation_id = "session-script-elicitation"
    for raw_line in sys.stdin.buffer:
        if len(raw_line) > MAX_MESSAGE_BYTES:
            raise RuntimeError("MCP fixture request exceeds the size limit")
        message = json.loads(raw_line)
        if not isinstance(message, dict):
            raise RuntimeError("MCP fixture received a non-object message")
        method = message.get("method")
        request_id = message.get("id")
        if method == "initialize":
            params = message.get("params")
            protocol_version = (
                params.get("protocolVersion")
                if isinstance(params, dict)
                else "2025-06-18"
            )
            recorder.add("initialized", protocolVersion=protocol_version)
            result(
                request_id,
                {
                    "protocolVersion": protocol_version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": "session-script-elicitation-test",
                        "version": "0.1.0",
                    },
                },
            )
        elif method == "notifications/initialized":
            recorder.add("initializedNotification")
        elif method == "tools/list":
            result(
                request_id,
                {
                    "tools": [
                        {
                            "name": "elicit",
                            "description": "Request a confirmation from the client.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {},
                                "additionalProperties": False,
                            },
                        }
                    ]
                },
            )
        elif method == "tools/call":
            pending_tool_call_id = request_id
            recorder.add("toolCalled")
            send(
                {
                    "jsonrpc": "2.0",
                    "id": elicitation_id,
                    "method": "elicitation/create",
                    "params": {
                        "mode": "form",
                        "message": "Allow the session-script MCP request?",
                        "requestedSchema": {
                            "type": "object",
                            "properties": {
                                "confirmed": {
                                    "type": "boolean",
                                    "title": "Confirm",
                                }
                            },
                            "required": ["confirmed"],
                        },
                    },
                }
            )
        elif request_id == elicitation_id:
            response = message.get("result")
            recorder.add(
                "elicitationAnswered",
                action=response.get("action") if isinstance(response, dict) else None,
            )
            if pending_tool_call_id is None:
                raise RuntimeError("MCP elicitation response has no pending tool call")
            result(
                pending_tool_call_id,
                {
                    "content": [
                        {
                            "type": "text",
                            "text": "session-script elicitation completed",
                        }
                    ],
                    "isError": False,
                },
            )
            pending_tool_call_id = None
        elif request_id is not None:
            result(request_id, {})
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
