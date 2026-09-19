#!/usr/bin/env python3
"""Deterministic external observer/responder for the session-script E2E."""

import argparse
import json
import os
from pathlib import Path
import sys
import time
from typing import Any
from typing import Optional

from session_script_sdk import RpcError
from session_script_sdk import SessionScriptClient


class Recorder:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.count = 0

    def add(self, event: str, **fields: object) -> None:
        if self.count >= 128:
            raise RpcError("test event limit exceeded")
        value = {"event": event, **fields}
        encoded = json.dumps(value, separators=(",", ":"), sort_keys=True)
        if len(encoded.encode()) > 4096:
            raise RpcError("test event exceeds the size limit")
        with self.path.open("a", encoding="utf-8") as output:
            output.write(encoded + "\n")
        self.count += 1


class Client:
    def __init__(
        self, endpoint: Optional[str], timeout: float, recorder: Recorder
    ) -> None:
        self.rpc = (
            SessionScriptClient.connect_websocket(endpoint, timeout)
            if endpoint
            else SessionScriptClient.from_host_child()
        )
        self.recorder = recorder
        self.rpc.set_notification_handler(self.handle_notification)
        self.registration_id: Optional[str] = None
        self.thread_id: Optional[str] = None
        self.active_turn_id: Optional[str] = None
        self.turn_steered = False
        self.turn_completed = False
        self.extension_commands_ready = False

    def initialize(self) -> None:
        self.rpc.initialize(
            "session-script-test-extension",
            "Session script test extension",
            "0.1.0",
        )

    def request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        return self.rpc.request(method, params)

    def request_error(self, method: str, params: dict[str, Any]) -> str:
        return self.rpc.request_error(method, params)

    def handle_next_message(self) -> None:
        self.rpc.handle_message(self.rpc.receive_message())

    def enable_server_request_handling(self) -> None:
        self.rpc.set_server_request_handler(self.handle_server_request)

    def handle_server_request(self, message: dict[str, Any]) -> dict[str, Any]:
        method = message.get("method")
        params = message.get("params")
        if not isinstance(method, str) or not isinstance(params, dict):
            raise RpcError("app-server sent an invalid server request")
        self.recorder.add("serverRequest", method=method)
        if method == "item/extensionInteraction/request":
            surface = params.get("surface")
            if not isinstance(surface, dict):
                raise RpcError("test extension interaction has no surface")
            surface_type = surface.get("type")
            action_id: Optional[str] = None
            values: dict[str, object] = {}
            sensitive_fields: list[dict[str, object]] = []
            if surface_type == "form":
                submit = surface.get("submit")
                fields = surface.get("fields")
                if not isinstance(submit, dict) or not isinstance(
                    submit.get("id"), str
                ):
                    raise RpcError("test extension form has no submit action")
                if not isinstance(fields, list):
                    raise RpcError("test extension form has no fields")
                action_id = submit["id"]
                for field in fields:
                    if not isinstance(field, dict) or not isinstance(
                        field.get("id"), str
                    ):
                        raise RpcError("test extension form has an invalid field")
                    if field.get("type") == "text":
                        values[field["id"]] = (
                            "session-script-sensitive-value"
                            if field.get("sensitive") is True
                            else "session-script-value"
                        )
                    elif field.get("type") == "boolean":
                        values[field["id"]] = bool(field.get("value"))
                    if field.get("sensitive") is True:
                        sensitive_fields.append(
                            {
                                "id": field["id"],
                                "defaultEmpty": field.get("value") == "",
                            }
                        )
            elif surface_type == "menu":
                items = surface.get("items")
                first_item = items[0] if isinstance(items, list) and items else None
                action = (
                    first_item.get("action") if isinstance(first_item, dict) else None
                )
                if not isinstance(action, dict) or not isinstance(
                    action.get("id"), str
                ):
                    raise RpcError("test extension menu has no selectable action")
                action_id = action["id"]
            elif surface_type == "confirmation":
                actions = surface.get("actions")
                if not isinstance(actions, list) or not actions:
                    raise RpcError(
                        "test extension confirmation has no selectable action"
                    )
                selected = next(
                    (
                        action
                        for action in actions
                        if isinstance(action, dict)
                        and action.get("id") == "approve-always"
                    ),
                    actions[0],
                )
                if not isinstance(selected, dict) or not isinstance(
                    selected.get("id"), str
                ):
                    raise RpcError("test extension action has no id")
                action_id = selected["id"]
            elif surface_type != "notice":
                raise RpcError(f"unexpected extension surface type: {surface_type}")
            outcome = "dismissed" if surface_type == "notice" else "accepted"
            result = {
                "extensionId": params.get("extensionId"),
                "interactionId": params.get("interactionId"),
                "continuation": params.get("continuation"),
                "stateRevision": params.get("stateRevision"),
                "outcome": outcome,
                "action": {"id": action_id} if action_id is not None else None,
                "values": values,
            }
            self.recorder.add(
                "extensionInteractionResponse",
                extensionId=params.get("extensionId"),
                interactionId=params.get("interactionId"),
                surfaceType=surface_type,
                actionId=action_id,
                valueKeys=sorted(values),
                sensitiveFields=sensitive_fields,
            )
        elif method == "item/tool/requestUserInput":
            questions = params.get("questions")
            if not isinstance(questions, list):
                raise RpcError("requestUserInput server request has no questions")
            answers: dict[str, dict[str, list[str]]] = {}
            for question in questions:
                if not isinstance(question, dict) or not isinstance(
                    question.get("id"), str
                ):
                    raise RpcError(
                        "requestUserInput server request has an invalid question"
                    )
                options = question.get("options")
                if not isinstance(options, list) or not options:
                    raise RpcError("test requestUserInput question has no options")
                first_option = options[0]
                if not isinstance(first_option, dict) or not isinstance(
                    first_option.get("label"), str
                ):
                    raise RpcError("test requestUserInput option has no label")
                answers[question["id"]] = {"answers": [first_option["label"]]}
            result = {"answers": answers}
        elif method == "item/commandExecution/requestApproval":
            result = {"decision": "decline"}
        elif method == "item/fileChange/requestApproval":
            result = {"decision": "decline"}
        elif method == "item/permissions/requestApproval":
            result = {"permissions": {}, "scope": "turn"}
        elif method == "mcpServer/elicitation/request":
            result = {"action": "decline", "content": None, "_meta": None}
        else:
            raise RpcError(f"unexpected server request: {method}")
        self.recorder.add("serverResponse", method=method)
        return result

    def handle_notification(self, message: dict[str, Any]) -> None:
        method = message.get("method")
        params = message.get("params")
        if not isinstance(method, str) or not isinstance(params, dict):
            return
        if method == "item/agentMessage/delta":
            self.recorder.add("delta", delta=params.get("delta"))
        elif method == "item/completed":
            item = params.get("item")
            if isinstance(item, dict):
                self.recorder.add(
                    "completed",
                    itemType=item.get("type"),
                    text=item.get("text"),
                    clientId=item.get("clientId"),
                    content=item.get("content"),
                )
        elif method == "turn/started":
            turn = params.get("turn")
            self.recorder.add(
                "turnStartedNotification",
                active=isinstance(turn, dict),
                status=turn.get("status") if isinstance(turn, dict) else None,
            )
        elif method == "turn/completed":
            turn = params.get("turn")
            self.turn_completed = True
            self.recorder.add(
                "turnCompleted",
                status=turn.get("status") if isinstance(turn, dict) else None,
            )
        elif method == "sessionExtension/message":
            self.recorder.add(
                "sessionExtensionMessage",
                threadId=params.get("threadId"),
                extensionName=params.get("extensionName"),
                level=params.get("level"),
                message=params.get("message"),
            )
        elif method == "script/sessionUpdated":
            session = params.get("session")
            self.recorder.add(
                "sessionUpdated",
                revision=params.get("revision"),
                cwd=session.get("cwd") if isinstance(session, dict) else None,
                title=session.get("title") if isinstance(session, dict) else None,
                projectName=(
                    session.get("projectName") if isinstance(session, dict) else None
                ),
                projectRoot=(
                    session.get("projectRoot") if isinstance(session, dict) else None
                ),
            )
        elif method == "script/promptOpened":
            prompt_id = params.get("promptId")
            lease = params.get("responseLease")
            can_respond = params.get("canRespond")
            request = params.get("request")
            self.recorder.add(
                "promptOpened",
                promptId=prompt_id,
                kind=params.get("kind"),
                requestMethod=(
                    request.get("method") if isinstance(request, dict) else None
                ),
                canRespond=can_respond,
                hasResponseLease=isinstance(lease, str),
            )
            snapshot = self.rpc.read(self._registration_id()).get("snapshot")
            if not isinstance(snapshot, dict):
                raise RpcError("script/read returned no snapshot during an open prompt")
            pending_prompts = snapshot.get("pendingPrompts")
            self.recorder.add(
                "promptRead",
                promptId=prompt_id,
                kind=params.get("kind"),
                activeTurn=isinstance(snapshot.get("turn"), dict),
                pendingPromptCount=(
                    len(pending_prompts) if isinstance(pending_prompts, list) else None
                ),
            )
            if (
                can_respond is True
                and isinstance(prompt_id, str)
                and isinstance(lease, str)
            ):
                request_params = (
                    request.get("params") if isinstance(request, dict) else None
                )
                questions = (
                    request_params.get("questions")
                    if isinstance(request_params, dict)
                    else None
                )
                question_ids = {
                    question.get("id")
                    for question in questions or []
                    if isinstance(question, dict)
                }
                if "fallback_path" in question_ids:
                    self.recorder.add("responseDeferred", promptId=prompt_id)
                    return
                self.steer_active_turn()
                self.recorder.add(
                    "invalidResponseRejected",
                    rejected=bool(
                        self.request_error(
                            "script/respond",
                            {
                                "registrationId": self._registration_id(),
                                "promptId": prompt_id,
                                "responseLease": lease,
                                "response": {
                                    "kind": "requestUserInput",
                                    "answers": {
                                        "confirm_path": {"answers": ["not-an-option"]}
                                    },
                                },
                            },
                        )
                    ),
                )
                self.rpc.respond(
                    self._registration_id(),
                    prompt_id,
                    lease,
                    {
                        "kind": "requestUserInput",
                        "answers": {"confirm_path": {"answers": ["Yes"]}},
                    },
                )
                self.recorder.add("responded", promptId=prompt_id)
        elif method == "script/promptClosed":
            self.recorder.add("promptClosed", reason=params.get("reason"))
        elif method == "script/resyncRequired":
            self.recorder.add("resyncRequired")
        elif method == "sessionExtension/commandsUpdated":
            commands = params.get("commands")
            self.extension_commands_ready = isinstance(commands, list) and bool(
                commands
            )
            self.recorder.add(
                "extensionCommandsUpdated",
                threadId=params.get("threadId"),
                commands=commands,
            )

    def _registration_id(self) -> str:
        if self.registration_id is None:
            raise RpcError("received a response prompt before registration completed")
        return self.registration_id

    def steer_active_turn(self) -> None:
        if self.turn_steered:
            return
        if self.thread_id is None or self.active_turn_id is None:
            raise RpcError(
                "received the primary response prompt before turn/start returned"
            )
        steer_result = self.request(
            "turn/steer",
            {
                "threadId": self.thread_id,
                "input": [{"type": "text", "text": "SESSION_SCRIPT_E2E_STEER"}],
                "clientUserMessageId": "session-script-e2e-steer",
                "expectedTurnId": self.active_turn_id,
            },
        )
        self.turn_steered = True
        self.recorder.add("turnSteered", turnId=steer_result.get("turnId"))

    def close(self) -> None:
        self.rpc.close()


def write_ready(path: Path, **value: object) -> None:
    path.write_text(json.dumps(value, separators=(",", ":")), encoding="utf-8")


def register(client: Client, args: argparse.Namespace, thread_id: str) -> None:
    capabilities = ["userInput.send", "prompt.requestUserInput.respond"]
    prompt_kinds = [
        "requestUserInput",
        "extensionInteraction",
        "commandExecutionApproval",
        "fileChangeApproval",
        "permissionsApproval",
        "mcpElicitation",
    ]
    result = client.rpc.register(
        thread_id,
        args.script_id,
        args.role,
        "0.1.0",
        {
            "modelResponseDeltas": True,
            "modelResponseCompleted": True,
            "userMessages": True,
            "turnCompleted": True,
            "prompts": prompt_kinds,
            "sessionUpdates": True,
        },
        capabilities,
    )
    registration_id = result.get("registrationId")
    if not isinstance(registration_id, str):
        raise RpcError("script/register returned no registrationId")
    client.registration_id = registration_id
    client.thread_id = thread_id
    client.recorder.add(
        "registered",
        grantedCapabilities=result.get("grantedCapabilities"),
        hasSnapshot=isinstance(result.get("snapshot"), dict),
    )
    registration_snapshot = result.get("snapshot")
    if not isinstance(registration_snapshot, dict):
        raise RpcError("script/register returned no snapshot")
    registration_session = registration_snapshot.get("session")
    registration_thread = registration_snapshot.get("thread")
    client.recorder.add(
        "registrationSnapshot",
        revision=registration_snapshot.get("revision"),
        cwd=(
            registration_session.get("cwd")
            if isinstance(registration_session, dict)
            else None
        ),
        projectName=(
            registration_session.get("projectName")
            if isinstance(registration_session, dict)
            else None
        ),
        projectRoot=(
            registration_session.get("projectRoot")
            if isinstance(registration_session, dict)
            else None
        ),
        sessionId=(
            registration_session.get("sessionId")
            if isinstance(registration_session, dict)
            else None
        ),
        threadId=(
            registration_session.get("threadId")
            if isinstance(registration_session, dict)
            else None
        ),
        threadStatus=(
            registration_thread.get("status")
            if isinstance(registration_thread, dict)
            else None
        ),
        canAcceptDirectInput=(
            registration_thread.get("canAcceptDirectInput")
            if isinstance(registration_thread, dict)
            else None
        ),
    )
    read_result = client.rpc.read(registration_id)
    snapshot = read_result.get("snapshot")
    if not isinstance(snapshot, dict):
        raise RpcError("script/read returned no snapshot")
    pending_prompts = snapshot.get("pendingPrompts")
    client.recorder.add(
        "read",
        hasSnapshot=True,
        revision=snapshot.get("revision"),
        pendingPromptCount=(
            len(pending_prompts) if isinstance(pending_prompts, list) else None
        ),
    )
    client.recorder.add(
        "restricted",
        rejected=bool(
            client.request_error(
                "thread/read", {"threadId": thread_id, "includeTurns": False}
            )
        ),
    )
    client.recorder.add(
        "turnSettingsRejected",
        rejected=bool(
            client.request_error(
                "turn/start",
                {
                    "threadId": thread_id,
                    "input": [{"type": "text", "text": "must not start"}],
                    "cwd": str(args.session_cwd),
                },
            )
        ),
    )


def run_controller(args: argparse.Namespace) -> int:
    recorder = Recorder(args.log)
    client = Client(args.endpoint, args.timeout, recorder)
    try:
        client.initialize()
        if args.command in ("start-thread", "start-idle-thread"):
            client.enable_server_request_handling()
            thread = client.request("thread/start", {"cwd": str(args.session_cwd)}).get(
                "thread"
            )
            if not isinstance(thread, dict) or not isinstance(thread.get("id"), str):
                raise RpcError("thread/start returned no thread id")
            if args.command == "start-idle-thread":
                while not client.extension_commands_ready:
                    client.handle_next_message()
                write_ready(args.ready_file, threadId=thread["id"])
                recorder.add("idleThreadReady", threadId=thread["id"])
                return 0
            recorder.add(
                "externalRegisterRejected",
                rejected=bool(
                    client.request_error(
                        "script/register",
                        {
                            "threadId": thread["id"],
                            "script": {
                                "id": "session-script-responder",
                                "name": "external impostor",
                                "version": "0.1.0",
                            },
                            "subscriptions": {},
                            "requestedCapabilities": [],
                        },
                    )
                ),
            )
            write_ready(args.primary_thread_file, threadId=thread["id"])
            write_ready(args.ready_file, threadId=thread["id"])
            recorder.add("threadStarted", threadId=thread["id"])
            while not client.turn_completed:
                client.handle_next_message()
            recorder.add("controllerTurnCompleted")
        elif args.command == "update-cwd":
            client.request(
                "thread/settings/update",
                {"threadId": args.thread_id, "cwd": str(args.session_cwd)},
            )
            recorder.add("settingsUpdated")
        elif args.command == "update-title":
            client.request(
                "thread/name/set",
                {"threadId": args.thread_id, "name": args.name},
            )
            recorder.add("titleUpdated")
        elif args.command == "extension-list":
            result = client.request(
                "sessionExtension/list", {"threadId": args.thread_id}
            )
            recorder.add(
                "extensionListed",
                threadId=args.thread_id,
                commands=result.get("commands"),
            )
        elif args.command == "extension-invoke":
            params = {
                "threadId": args.thread_id,
                "extensionId": args.extension_id,
                "command": args.extension_command,
                "arguments": args.arguments,
            }
            recorder.add("extensionInvokeAttempt", **params)
            try:
                client.request("sessionExtension/command/invoke", params)
            except RpcError as error:
                recorder.add("extensionInvokeFailed", error=str(error), **params)
                raise
            recorder.add(
                "extensionInvoked",
                threadId=args.thread_id,
                arguments=args.arguments,
            )
        elif args.command == "mcp-tool-call":
            client.enable_server_request_handling()
            result = client.request(
                "mcpServer/tool/call",
                {
                    "threadId": args.thread_id,
                    "server": args.server,
                    "tool": args.tool,
                    "arguments": {},
                },
            )
            recorder.add(
                "mcpToolCalled",
                threadId=args.thread_id,
                content=result.get("content"),
            )
        else:
            client.request("thread/archive", {"threadId": args.thread_id})
            recorder.add("threadArchived", threadId=args.thread_id)
        return 0
    finally:
        client.close()


def start_turn(client: Client, recorder: Recorder, thread_id: str) -> None:
    turn_result = client.request(
        "turn/start",
        {
            "threadId": thread_id,
            "input": [{"type": "text", "text": "SESSION_SCRIPT_E2E_PROMPT"}],
            "clientUserMessageId": "session-script-e2e",
        },
    )
    turn = turn_result.get("turn")
    if not isinstance(turn, dict) or not isinstance(turn.get("id"), str):
        raise RpcError("turn/start returned no turn id")
    client.active_turn_id = turn["id"]
    recorder.add("turnStarted", turnId=turn["id"])


def run_script(args: argparse.Namespace) -> int:
    recorder = Recorder(args.log)
    client = Client(args.endpoint, args.timeout, recorder)
    try:
        client.initialize()
        thread_id = args.thread_id
        if thread_id is None:
            thread = client.request("thread/start", {"cwd": str(args.session_cwd)}).get(
                "thread"
            )
            if not isinstance(thread, dict) or not isinstance(thread.get("id"), str):
                raise RpcError("thread/start returned no thread id")
            thread_id = thread["id"]
        register(client, args, thread_id)
        write_ready(args.ready_file, threadId=thread_id, role=args.role)
        if args.start_file is not None:
            deadline = time.monotonic() + args.timeout
            while not args.start_file.exists():
                if time.monotonic() >= deadline:
                    raise RpcError("timed out waiting for start signal")
                time.sleep(0.05)
            start_turn(client, recorder, thread_id)
        deadline = time.monotonic() + args.timeout
        while not client.turn_completed:
            if time.monotonic() >= deadline:
                raise RpcError("timed out waiting for turn/completed")
            client.handle_next_message()
        client.rpc.unregister(client._registration_id())
        recorder.add("unregistered")
        return 0
    finally:
        client.close()


def run_child(args: argparse.Namespace) -> int:
    script_id = os.environ.get("XEDOC_SESSION_SCRIPT_ID")
    thread_id = os.environ.get("XEDOC_SESSION_SCRIPT_THREAD_ID")
    if not script_id or not thread_id:
        raise RpcError("host did not provide the session script scope")
    recorder = Recorder(args.log)
    if args.primary_thread_file is not None:
        deadline = time.monotonic() + args.timeout
        while not args.primary_thread_file.exists():
            if time.monotonic() >= deadline:
                raise RpcError("timed out waiting for the primary thread identity")
            time.sleep(0.05)
        primary_thread = json.loads(
            args.primary_thread_file.read_text(encoding="utf-8")
        ).get("threadId")
        if thread_id != primary_thread:
            recorder.add(
                "secondaryHostSkipped",
                scriptId=script_id,
                threadId=thread_id,
                pid=os.getpid(),
            )
            return 0
    client = Client(None, args.timeout, recorder)
    try:
        client.initialize()
        recorder.add(
            "preRegistrationRejected",
            rejected=bool(client.request_error("config/read", {})),
        )
        child_args = argparse.Namespace(
            role=args.role,
            script_id=script_id,
            session_cwd=Path.cwd(),
        )
        register(client, child_args, thread_id)
        recorder.add(
            "hostScope", scriptId=script_id, threadId=thread_id, pid=os.getpid()
        )
        if args.start_file is not None:
            deadline = time.monotonic() + args.timeout
            while not args.start_file.exists():
                if time.monotonic() >= deadline:
                    raise RpcError("timed out waiting for start signal")
                time.sleep(0.05)
            start_turn(client, recorder, thread_id)
        deadline = time.monotonic() + args.timeout
        while not client.turn_completed:
            if time.monotonic() >= deadline:
                raise RpcError("timed out waiting for turn/completed")
            client.handle_next_message()
        client.rpc.unregister(client._registration_id())
        recorder.add("unregistered")
        return 0
    finally:
        client.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--endpoint")
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--log", type=Path, required=True)
    subparsers = parser.add_subparsers(dest="command", required=True)
    script = subparsers.add_parser("script")
    script.add_argument("--role", choices=("observer", "responder"), required=True)
    script.add_argument("--script-id", required=True)
    script.add_argument("--thread-id")
    script.add_argument("--session-cwd", type=Path, required=True)
    script.add_argument("--ready-file", type=Path, required=True)
    script.add_argument("--start-file", type=Path)
    controller = subparsers.add_parser("update-cwd")
    controller.add_argument("--thread-id", required=True)
    controller.add_argument("--session-cwd", type=Path, required=True)
    title = subparsers.add_parser("update-title")
    title.add_argument("--thread-id", required=True)
    title.add_argument("--name", required=True)
    archive = subparsers.add_parser("archive-thread")
    archive.add_argument("--thread-id", required=True)
    start = subparsers.add_parser("start-thread")
    start.add_argument("--session-cwd", type=Path, required=True)
    start.add_argument("--ready-file", type=Path, required=True)
    start.add_argument("--primary-thread-file", type=Path, required=True)
    idle = subparsers.add_parser("start-idle-thread")
    idle.add_argument("--session-cwd", type=Path, required=True)
    idle.add_argument("--ready-file", type=Path, required=True)
    extension_list = subparsers.add_parser("extension-list")
    extension_list.add_argument("--thread-id", required=True)
    extension_invoke = subparsers.add_parser("extension-invoke")
    extension_invoke.add_argument("--thread-id", required=True)
    extension_invoke.add_argument("--extension-id", required=True)
    extension_invoke.add_argument("--extension-command", required=True)
    extension_invoke.add_argument("arguments", nargs="*")
    mcp_tool_call = subparsers.add_parser("mcp-tool-call")
    mcp_tool_call.add_argument("--thread-id", required=True)
    mcp_tool_call.add_argument("--server", required=True)
    mcp_tool_call.add_argument("--tool", required=True)
    child = subparsers.add_parser("child")
    child.add_argument("--role", choices=("observer", "responder"), required=True)
    child.add_argument("--start-file", type=Path)
    child.add_argument("--primary-thread-file", type=Path)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        return (
            run_controller(args)
            if args.command
            in (
                "start-thread",
                "start-idle-thread",
                "update-cwd",
                "update-title",
                "archive-thread",
                "extension-list",
                "extension-invoke",
                "mcp-tool-call",
            )
            else run_child(args)
            if args.command == "child"
            else run_script(args)
        )
    except (OSError, RpcError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
