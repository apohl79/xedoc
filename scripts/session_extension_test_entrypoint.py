#!/usr/bin/env python3
"""Dual one-shot and persistent fixture for session-extension E2E coverage."""

import argparse
import json
import os
from pathlib import Path
import sys
from typing import Any

from session_script_sdk import RpcError
from session_script_sdk import SessionScriptClient


PROTOCOL = "xedoc.script/v1"
SETUP_CONTINUATION = "session-extension-setup"
COMMAND_CONTINUATION = "session-extension-command"


class Recorder:
    def __init__(self, path: Path) -> None:
        self.path = path

    def add(self, event: str, **fields: object) -> None:
        encoded = json.dumps(
            {"event": event, **fields}, separators=(",", ":"), sort_keys=True
        )
        if len(encoded.encode()) > 4096:
            raise RuntimeError("extension fixture event exceeds the size limit")
        fd = os.open(self.path, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
        try:
            os.write(fd, (encoded + "\n").encode())
        finally:
            os.close(fd)


def action(action_id: str, label: str) -> dict[str, object]:
    return {
        "id": action_id,
        "opens": None,
        "hostAction": None,
        "label": label,
        "keyBindings": [],
        "context": None,
        "value": None,
    }


def interaction_result(
    request: dict[str, Any],
    interaction_id: str,
    continuation: str,
    surface: dict[str, object],
) -> dict[str, object]:
    return {
        "protocol": PROTOCOL,
        "requestId": request["requestId"],
        "result": {
            "kind": "interaction",
            "interaction": {
                "id": interaction_id,
                "continuation": continuation,
                "stateRevision": "1",
                "surface": surface,
            },
        },
    }


def complete(request: dict[str, Any], summary: str) -> dict[str, object]:
    return {
        "protocol": PROTOCOL,
        "requestId": request["requestId"],
        "result": {"kind": "complete", "summary": summary},
    }


def run_one_shot(recorder: Recorder) -> int:
    request = json.load(sys.stdin)
    if not isinstance(request, dict) or request.get("protocol") != PROTOCOL:
        raise RuntimeError("host sent an invalid extension request")
    method = request.get("method")
    context = request.get("context")
    params = request.get("params")
    if not isinstance(context, dict) or not isinstance(params, dict):
        raise RuntimeError("extension request is missing context or params")
    recorder.add(
        "oneShotRequest",
        method=method,
        extensionId=context.get("extensionId"),
        pluginId=context.get("pluginId"),
        threadId=context.get("threadId"),
    )
    if method == "extension.setup.open":
        response = interaction_result(
            request,
            "session-extension-setup-form",
            SETUP_CONTINUATION,
            {
                "type": "form",
                "id": "session-extension-credentials",
                "title": "Configure signal extension",
                "subtitle": "The token must stay masked.",
                "fields": [
                    {
                        "type": "text",
                        "id": "api-token",
                        "label": "API token",
                        "description": "A sensitive setup value.",
                        "value": "",
                        "maxBytes": 128,
                        "sensitive": True,
                    }
                ],
                "submit": action("save-setup", "Save"),
                "cancel": None,
            },
        )
    elif method == "extension.command.invoke":
        arguments = params.get("arguments")
        if not isinstance(arguments, list) or not all(
            isinstance(argument, str) for argument in arguments
        ):
            raise RuntimeError("command invocation has invalid arguments")
        recorder.add(
            "commandInvoked",
            threadId=context.get("threadId"),
            command=params.get("command"),
            arguments=arguments,
        )
        response = interaction_result(
            request,
            "session-extension-command-confirmation",
            COMMAND_CONTINUATION,
            {
                "type": "confirmation",
                "title": "Continue signal command?",
                "body": "Exercise command interaction continuation.",
                "details": [],
                "sections": [],
                "actions": [action("continue-command", "Continue")],
                "override": None,
            },
        )
    elif method == "interaction.respond":
        continuation = params.get("continuation")
        if continuation == SETUP_CONTINUATION:
            values = params.get("values")
            token = values.get("api-token") if isinstance(values, dict) else None
            if not isinstance(token, str) or not token:
                raise RuntimeError("setup response did not include the sensitive token")
            recorder.add(
                "setupResponded",
                threadId=context.get("threadId"),
                actionId=(
                    params.get("action", {}).get("id")
                    if isinstance(params.get("action"), dict)
                    else None
                ),
                sensitiveValueBytes=len(token.encode()),
            )
            response = complete(request, "signal extension configured")
        elif continuation == COMMAND_CONTINUATION:
            recorder.add(
                "commandResponded",
                threadId=context.get("threadId"),
                actionId=(
                    params.get("action", {}).get("id")
                    if isinstance(params.get("action"), dict)
                    else None
                ),
            )
            response = complete(request, "signal command completed")
        else:
            raise RuntimeError("interaction response has an unknown continuation")
    else:
        raise RuntimeError(f"unsupported extension method: {method}")
    json.dump(response, sys.stdout, separators=(",", ":"))
    sys.stdout.write("\n")
    sys.stdout.flush()
    return 0


def run_persistent(recorder: Recorder, script_id: str, thread_id: str) -> int:
    client = SessionScriptClient.from_host_child()

    def handle_notification(message: dict[str, Any]) -> None:
        method = message.get("method")
        params = message.get("params")
        if not isinstance(params, dict):
            return
        if method == "script/sessionUpdated":
            session = params.get("session")
            recorder.add(
                "persistentSessionUpdated",
                threadId=thread_id,
                title=session.get("title") if isinstance(session, dict) else None,
                cwd=session.get("cwd") if isinstance(session, dict) else None,
            )
        elif method == "item/agentMessage/delta":
            recorder.add(
                "persistentDelta", threadId=thread_id, delta=params.get("delta")
            )
        elif method == "item/completed":
            item = params.get("item")
            if isinstance(item, dict) and item.get("type") == "agentMessage":
                recorder.add(
                    "persistentCompleted",
                    threadId=thread_id,
                    text=item.get("text"),
                )
        elif method == "turn/completed":
            recorder.add("persistentTurnCompleted", threadId=thread_id)

    client.set_notification_handler(handle_notification)
    try:
        client.initialize(
            "session-extension-test",
            "Session extension test",
            "0.1.0",
        )
        result = client.register(
            thread_id,
            script_id,
            "Persistent signal extension",
            "0.1.0",
            {
                "modelResponseDeltas": True,
                "modelResponseCompleted": True,
                "turnCompleted": True,
                "prompts": [],
                "sessionUpdates": True,
            },
            ["userInput.send"],
        )
        recorder.add(
            "persistentRegistered",
            threadId=thread_id,
            scriptId=script_id,
            pid=os.getpid(),
            grantedCapabilities=result.get("grantedCapabilities"),
            hasSnapshot=isinstance(result.get("snapshot"), dict),
        )
        while True:
            client.handle_message(client.receive_message())
    except RpcError as error:
        recorder.add("persistentDisconnected", threadId=thread_id, error=str(error))
        return 0
    finally:
        client.close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--log", type=Path, required=True)
    args = parser.parse_args()
    recorder = Recorder(args.log)
    script_id = os.environ.get("XEDOC_SESSION_SCRIPT_ID")
    thread_id = os.environ.get("XEDOC_SESSION_SCRIPT_THREAD_ID")
    try:
        if script_id and thread_id:
            return run_persistent(recorder, script_id, thread_id)
        return run_one_shot(recorder)
    except (OSError, RuntimeError, ValueError) as error:
        recorder.add("extensionError", message=str(error))
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
