#!/usr/bin/env python3
"""Deterministic Responses mocks and controller for remote-agent pairing E2E.

Run two mocks, sharing ``--state-file``, one for each app server.  The source
mock drives a remote-session lifecycle from a real model turn.  The target
mock deliberately leaves its remote turn pending until the target controller
observes its cancellation.  Run one controller connected to both Unix sockets.

Example::

  python3 scripts/remote_agent_pairing_e2e.py mock --role source \\
    --port-file /tmp/source.port --request-log /tmp/source.jsonl --state-file /tmp/state.json
  python3 scripts/remote_agent_pairing_e2e.py mock --role target \\
    --port-file /tmp/target.port --request-log /tmp/target.jsonl --state-file /tmp/state.json
  python3 scripts/remote_agent_pairing_e2e.py controller \\
    --source-socket /tmp/source.sock --target-socket /tmp/target.sock --cwd / \\
    --ready-file /tmp/ready.json --evidence-file /tmp/evidence.jsonl --state-file /tmp/state.json

All listeners bind only to 127.0.0.1.  The JSONL files are bounded evidence for
an outer tmux wrapper; no credentials or external network endpoints are used.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import fcntl
import json
import os
from http.server import BaseHTTPRequestHandler
from http.server import ThreadingHTTPServer
from pathlib import Path
import queue
import signal
import sys
import threading
import time
from typing import Any

from session_script_sdk import RpcError
from session_script_sdk import SessionScriptClient


SOURCE_MARKER = "REMOTE_AGENT_PAIRING_E2E_SOURCE"
RESULT_MARKER = "list the files in /tmp"
RESULT_OUTPUT_MARKER = "remote-agent-e2e-tmp-entry"
TARGET_MARKER = "REMOTE_AGENT_PAIRING_E2E_TARGET"
TUI_FOLLOW_UP_MARKER = "REMOTE_AGENT_PROJECTION_E2E_TUI_FOLLOW_UP"
TUI_FOLLOW_UP_OUTPUT_MARKER = "target tui follow-up"
TARGET_LIVE_OUTPUT_MARKER = "remote-agent pairing E2E live output"
TARGET_ITEMS_MARKER = "REMOTE_AGENT_ITEMS_E2E"
TARGET_ITEMS_REASONING = "items e2e reasoning"
TARGET_ITEMS_COMMAND_OUTPUT = "items-e2e-command-output"
TARGET_ITEMS_FILE = "items-e2e-file.txt"
TARGET_ITEMS_DONE = "items e2e done"
WORKSPACE_ID = "workspace_root"
MAX_EVENTS = 256
MAX_EVENT_BYTES = 8192
MAX_ACTIVITY_BYTES = 64
MAX_BACKGROUND_RESPONSES = 1
STATE_POLL_SECONDS = 0.05
STATE_SYNC_TIMEOUT = 30.0
_STATE_PROCESS_LOCK = threading.RLock()
_SOURCE_STAGES = (
    "grant",
    "start",
    "wait-start",
    "send-result",
    "wait-result",
    "send",
    "wait-send",
    "cancel",
    "wait-cancel",
)


def _json(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


class Recorder:
    def __init__(self, path: Path) -> None:
        self.path = path
        self.count = 0
        self.lock = threading.Lock()

    def add(self, event: str, **fields: object) -> None:
        line = _json({"event": event, **fields})
        if len(line.encode()) > MAX_EVENT_BYTES:
            raise RuntimeError("pairing E2E evidence event is too large")
        with self.lock:
            if self.count >= MAX_EVENTS:
                raise RuntimeError("pairing E2E evidence event limit exceeded")
            with self.path.open("a", encoding="utf-8") as output:
                output.write(line + "\n")
            self.count += 1


@contextmanager
def _state_file_lock(path: Path, exclusive: bool) -> Any:
    lock_path = path.with_suffix(path.suffix + ".lock")
    with _STATE_PROCESS_LOCK, lock_path.open("a+", encoding="utf-8") as lock_file:
        operation = fcntl.LOCK_EX if exclusive else fcntl.LOCK_SH
        fcntl.flock(lock_file.fileno(), operation)
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
        raise RuntimeError(f"invalid E2E state file: {path}") from error
    if not isinstance(value, dict):
        raise RuntimeError("E2E state must be a JSON object")
    return value


def _state_read(path: Path) -> dict[str, Any]:
    deadline = time.monotonic() + 1.0
    while True:
        try:
            with _state_file_lock(path, exclusive=False):
                return _state_read_unlocked(path)
        except RuntimeError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(STATE_POLL_SECONDS)


def _state_update(path: Path, **updates: object) -> dict[str, Any]:
    with _state_file_lock(path, exclusive=True):
        state = _state_read_unlocked(path)
        state.update(updates)
        _state_write_unlocked(path, state)
        return state


def _state_write_unlocked(path: Path, state: dict[str, Any]) -> None:
    temporary = path.with_name(
        f".{path.name}.{os.getpid()}.{threading.get_ident()}.tmp"
    )
    try:
        temporary.write_text(_json(state) + "\n", encoding="utf-8")
        temporary.replace(path)
    finally:
        temporary.unlink(missing_ok=True)


def _wait_for_state(path: Path, key: str, timeout: float = STATE_SYNC_TIMEOUT) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = _state_read(path).get(key)
        if value is not None:
            return value
        time.sleep(STATE_POLL_SECONDS)
    raise RuntimeError(f"timed out waiting for E2E state {key}")


def _shell_call(
    call_id: str, command: str, reasoning: str | None = None
) -> list[dict[str, object]]:
    response_id = f"pairing-e2e-{call_id}"
    events = [_event("response.created", response={"id": response_id})]
    if reasoning is not None:
        events.append(
            _event(
                "response.output_item.done",
                item={
                    "type": "reasoning",
                    "id": f"{response_id}-reasoning",
                    "summary": [{"type": "summary_text", "text": reasoning}],
                },
            )
        )
    events.append(
        _event(
            "response.output_item.done",
            item={
                "type": "function_call",
                "call_id": call_id,
                "name": "shell_command",
                "arguments": _json({"command": command}),
            },
        )
    )
    events.append(_completed(response_id))
    return events


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
    response_id = f"pairing-e2e-{call_id}"
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
    response_id = "pairing-e2e-final"
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


def _call_outputs(request: dict[str, Any]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for item in request.get("input", []):
        if not isinstance(item, dict) or not isinstance(item.get("call_id"), str):
            continue
        if item.get("type") not in {"function_call_output", "custom_tool_call_output"}:
            continue
        result[item["call_id"]] = _decode_output(item)
    return result


def _call_output_ids(request: dict[str, Any]) -> list[str]:
    result = []
    for item in request.get("input", []):
        if not isinstance(item, dict) or not isinstance(item.get("call_id"), str):
            continue
        if item.get("type") in {
            "function_call_output",
            "custom_tool_call_output",
        }:
            result.append(item["call_id"])
    return result


def _decode_output(item: dict[str, Any]) -> Any:
    for key in ("output", "content"):
        value = item.get(key)
        decoded = _decode_json(value)
        if decoded is not None:
            return decoded
    return item


def _decode_json(value: Any) -> Any | None:
    if isinstance(value, str):
        try:
            return json.loads(value)
        except json.JSONDecodeError:
            return None
    if isinstance(value, list):
        for item in value:
            if isinstance(item, dict):
                decoded = _decode_json(item.get("text") or item.get("content"))
                if decoded is not None:
                    return decoded
    if isinstance(value, dict):
        return value
    return None


def _find(value: Any, key: str) -> str | None:
    if isinstance(value, dict):
        candidate = value.get(key)
        if isinstance(candidate, str) and candidate:
            return candidate
        for child in value.values():
            found = _find(child, key)
            if found is not None:
                return found
    elif isinstance(value, list):
        for child in value:
            found = _find(child, key)
            if found is not None:
                return found
    return None


def _managed_host_id(state_file: Path) -> str:
    host_id = _state_read(state_file).get("managedHostId")
    if not isinstance(host_id, str) or not host_id:
        raise RuntimeError("managed peer host ID is missing from E2E state")
    return host_id


def _grant_replaced(value: Any, host_id: str) -> bool:
    if isinstance(value, dict) and "contentItems" in value:
        if value.get("success") is not True:
            return False
        content_items = value["contentItems"]
        if not isinstance(content_items, list):
            return False
        for item in content_items:
            if not isinstance(item, dict) or item.get("type") != "inputText":
                continue
            decoded = _decode_json(item.get("text"))
            if decoded is not None:
                return _grant_replaced(decoded)
        return False
    if not isinstance(value, dict) or value.get("status") != "ok":
        return False
    result = value.get("result")
    if not isinstance(result, dict) or result.get("hostId") != host_id:
        return False
    revision = result.get("revision")
    if not isinstance(revision, int) or isinstance(revision, bool) or revision < 1:
        return False
    grants = result.get("data")
    if not isinstance(grants, list) or len(grants) != 3:
        return False
    return {
        (grant.get("scope"), grant.get("workspaceId"))
        for grant in grants
        if isinstance(grant, dict)
    } == {
        ("sessionRead", WORKSPACE_ID),
        ("sessionWrite", WORKSPACE_ID),
        ("cancellation", WORKSPACE_ID),
    }


def _peer_session_provider_result(value: Any) -> dict[str, Any]:
    if (
        not isinstance(value, dict)
        or "success" in value
        or "contentItems" in value
        or value.get("status") != "ok"
    ):
        raise RuntimeError(
            f"remote peer session result was not a successful provider response: {value!r}"
        )
    result = value.get("result")
    if not isinstance(result, dict):
        raise RuntimeError("remote peer session result had invalid provider result")
    return result


def _peer_session_result(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise RuntimeError(
            "remote peer session result was not a successful extension response"
        )
    has_legacy_keys = "success" in value or "contentItems" in value
    has_provider_keys = "status" in value or "result" in value
    if has_legacy_keys:
        if has_provider_keys:
            raise RuntimeError(
                "remote peer session result mixed extension and provider responses"
            )
        if value.get("success") is not True:
            raise RuntimeError(
                "remote peer session result was not a successful extension response"
            )
        content_items = value.get("contentItems")
        if not isinstance(content_items, list) or len(content_items) != 1:
            raise RuntimeError(
                "remote peer session result had invalid extension content"
            )
        content = content_items[0]
        if not isinstance(content, dict) or content.get("type") != "inputText":
            raise RuntimeError(
                "remote peer session result had invalid extension content item"
            )
        text = content.get("text")
        if not isinstance(text, str) or len(text.encode()) > MAX_EVENT_BYTES:
            raise RuntimeError(
                "remote peer session result content was invalid or too large"
            )
        decoded = _decode_json(text)
        if not isinstance(decoded, dict):
            raise RuntimeError(
                "remote peer session result content was not a JSON object"
            )
        return _peer_session_provider_result(decoded)
    return _peer_session_provider_result(value)


def _source_stage_for_request(
    request: dict[str, Any], state_file: Path
) -> tuple[str, dict[str, Any]]:
    output_ids = _call_output_ids(request)
    outputs = _call_outputs(request)
    with _state_file_lock(state_file, exclusive=True):
        state = _state_read_unlocked(state_file)
        stage = state.get("sourceStage")
        if stage is None:
            if output_ids or SOURCE_MARKER not in _json(request):
                raise RuntimeError(
                    "source E2E state machine did not receive its initial turn"
                )
            state["sourceStage"] = _SOURCE_STAGES[0]
            _state_write_unlocked(state_file, state)
            return _SOURCE_STAGES[0], outputs
        if not isinstance(stage, str) or stage not in _SOURCE_STAGES:
            raise RuntimeError(f"source E2E state machine has invalid stage {stage!r}")
        expected_ids = list(_SOURCE_STAGES[: _SOURCE_STAGES.index(stage) + 1])
        if output_ids != expected_ids:
            raise RuntimeError(
                "source Responses outputs skipped, duplicated, or arrived out of order"
            )
        return stage, outputs


def _source_background_events(
    request: dict[str, Any], state_file: Path
) -> tuple[str, list[dict[str, object]]] | None:
    output_ids = _call_output_ids(request)
    with _state_file_lock(state_file, exclusive=True):
        state = _state_read_unlocked(state_file)
        if state.get("sourceStage") != "complete":
            return None
        if output_ids:
            raise RuntimeError(
                "source background Responses request continued an unexpected tool call"
            )
        count = state.get("sourceBackgroundResponseCount", 0)
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            raise RuntimeError("source background Responses count is invalid")
        if count >= MAX_BACKGROUND_RESPONSES:
            raise RuntimeError("source background Responses request limit exceeded")
        state["sourceBackgroundResponseCount"] = count + 1
        _state_write_unlocked(state_file, state)
    return "background", _assistant("Complete")


def _advance_source_stage(state_file: Path, stage: str, **updates: object) -> None:
    with _state_file_lock(state_file, exclusive=True):
        state = _state_read_unlocked(state_file)
        if state.get("sourceStage") != stage:
            raise RuntimeError("source E2E state machine advanced concurrently")
        stage_index = _SOURCE_STAGES.index(stage)
        state["sourceStage"] = (
            _SOURCE_STAGES[stage_index + 1]
            if stage_index + 1 < len(_SOURCE_STAGES)
            else "complete"
        )
        state.update(updates)
        _state_write_unlocked(state_file, state)


ACTIVITY_SUMMARY_PROMPT = "Summarize what this coding agent is currently working on"
ACTIVITY_SUMMARY_TEXT = "Watching remote agent output"


def _is_activity_summary_request(request: dict[str, Any]) -> bool:
    return ACTIVITY_SUMMARY_PROMPT in json.dumps(request.get("input", []))


def _activity_summary_events() -> list[dict[str, object]]:
    response_id = "pairing-e2e-activity-summary"
    return [
        _event("response.created", response={"id": response_id}),
        _event("response.output_text.delta", delta=ACTIVITY_SUMMARY_TEXT),
        _completed(response_id),
    ]


def _source_events(
    request: dict[str, Any], state_file: Path
) -> tuple[str, list[dict[str, object]]]:
    if _is_activity_summary_request(request):
        return "activity-summary", _activity_summary_events()
    host_id = _managed_host_id(state_file)
    background_events = _source_background_events(request, state_file)
    if background_events is not None:
        return background_events
    stage, outputs = _source_stage_for_request(request, state_file)
    if stage == "grant":
        if not outputs:
            return "grant", _function_call(
                "grant",
                "remote_host_grant_set",
                {
                    "hostId": host_id,
                    "workspaceId": WORKSPACE_ID,
                    "scopes": ["sessionRead", "sessionWrite", "cancellation"],
                    "expiresAt": int(time.time()) + 3600,
                },
            )
        if not _grant_replaced(outputs["grant"], host_id):
            raise RuntimeError(
                "remote_host_grant_set did not replace managed host grants"
            )
        _advance_source_stage(state_file, stage, grantObserved=True)
        return "start", _function_call(
            "start",
            "remote_session_start",
            {"hostId": host_id, "workspaceId": WORKSPACE_ID, "relativePath": ""},
        )
    if stage == "start":
        operation_id = _find(_peer_session_result(outputs["start"]), "operationId")
        if operation_id is None:
            raise RuntimeError("remote_session_start result omitted operationId")
        _advance_source_stage(state_file, stage)
        return "wait-start", _function_call(
            "wait-start",
            "remote_session_wait",
            {"hostId": host_id, "operationId": operation_id, "timeoutSeconds": 1},
        )
    if stage == "wait-start":
        completed = _peer_session_result(outputs["wait-start"])
        thread_id = _find(completed, "threadId")
        if thread_id is None:
            raise RuntimeError(
                f"remote_session_start completion omitted threadId: {completed!r}"
            )
        _advance_source_stage(state_file, stage, targetThreadId=thread_id)
        return "send-result", _function_call(
            "send-result",
            "remote_session_send",
            {"hostId": host_id, "threadId": thread_id, "message": RESULT_MARKER},
        )
    if stage == "send-result":
        operation_id = _find(
            _peer_session_result(outputs["send-result"]), "operationId"
        )
        if operation_id is None:
            raise RuntimeError(
                "completed remote_session_send result omitted operationId"
            )
        _advance_source_stage(state_file, stage)
        return "wait-result", _function_call(
            "wait-result",
            "remote_session_wait",
            {"hostId": host_id, "operationId": operation_id, "timeoutSeconds": 5},
        )
    if stage == "wait-result":
        completed = _peer_session_result(outputs["wait-result"])
        if completed.get("state") != "completed":
            raise RuntimeError("remote completed task did not reach completed state")
        events = completed.get("events")
        if (
            not isinstance(events, list)
            or not events
            or events[-1] != {"type": "terminal", "status": "completed"}
            or any(
                not isinstance(event, dict)
                or event.get("type") != "progress"
                or event.get("status") != "running"
                for event in events[:-1]
            )
        ):
            raise RuntimeError(
                "remote completed task did not return running progress followed by a terminal completed event"
            )
        result = completed.get("result")
        if (
            not isinstance(result, dict)
            or result.get("status") != "completed"
            or not isinstance(result.get("outputText"), str)
            or RESULT_OUTPUT_MARKER not in result["outputText"]
        ):
            raise RuntimeError(
                "remote completed task did not return its structured result output"
            )
        thread_id = completed.get("threadId")
        turn_id = completed.get("turnId")
        operation = completed.get("operation")
        if not all(
            isinstance(value, str) and value
            for value in (thread_id, turn_id, operation)
        ):
            raise RuntimeError("remote completed task omitted terminal identifiers")
        completed_task_result = {
            "operation": operation,
            "state": "completed",
            "threadId": thread_id,
            "turnId": turn_id,
            "resultStatus": "completed",
            "outputText": result["outputText"],
            "events": events,
            "stopReason": completed.get("stopReason"),
        }
        if completed_task_result["stopReason"] != "terminal":
            raise RuntimeError(
                "remote completed task did not stop at its terminal event"
            )
        _advance_source_stage(
            state_file,
            stage,
            completedTaskResult=completed_task_result,
        )
        if _wait_for_state(state_file, "targetObserverSubscribed") is not True:
            raise RuntimeError(
                "target observer did not subscribe after the completed peer task"
            )
        return "send", _function_call(
            "send",
            "remote_session_send",
            {"hostId": host_id, "threadId": thread_id, "message": TARGET_MARKER},
        )
    if stage == "send":
        operation_id = _find(_peer_session_result(outputs["send"]), "operationId")
        if operation_id is None:
            raise RuntimeError("remote_session_send result omitted operationId")
        _advance_source_stage(state_file, stage)
        return "wait-send", _function_call(
            "wait-send",
            "remote_session_wait",
            {"hostId": host_id, "operationId": operation_id, "timeoutSeconds": 1},
        )
    if stage == "wait-send":
        wait_send = _peer_session_result(outputs["wait-send"])
        thread_id = _find(wait_send, "threadId")
        turn_id = _find(wait_send, "turnId")
        if thread_id is None or turn_id is None:
            raise RuntimeError(
                "remote_session_send completion omitted threadId or turnId"
            )
        events = wait_send.get("events")
        if (
            not isinstance(events, list)
            or not events
            or any(
                not isinstance(event, dict)
                or event.get("type") != "progress"
                or event.get("status") != "running"
                or not isinstance(event.get("activitySummary"), str)
                or not event["activitySummary"]
                for event in events
            )
        ):
            raise RuntimeError(
                "remote_session_wait did not return the expected running activity event"
            )
        _advance_source_stage(
            state_file,
            stage,
            progressEvents=events,
            targetThreadId=thread_id,
            targetTurnId=turn_id,
        )
        if (
            _state_read(state_file).get("requireGeneratedSummary") is True
            and _wait_for_state(state_file, "remoteSummaryGenerated") is not True
        ):
            raise RuntimeError(
                "the remote session never showed a model-generated activity summary"
            )
        return "cancel", _function_call(
            "cancel",
            "remote_session_cancel",
            {"hostId": host_id, "threadId": thread_id, "turnId": turn_id},
        )
    if stage == "cancel":
        operation_id = _find(_peer_session_result(outputs["cancel"]), "operationId")
        if operation_id is None:
            raise RuntimeError("remote_session_cancel result omitted operationId")
        _advance_source_stage(state_file, stage)
        return "wait-cancel", _function_call(
            "wait-cancel",
            "remote_session_wait",
            {"hostId": host_id, "operationId": operation_id, "timeoutSeconds": 5},
        )
    if stage == "wait-cancel":
        cancellation = _peer_session_result(outputs["wait-cancel"])
        if cancellation.get("state") != "cancelled":
            raise RuntimeError("remote session cancellation was not terminal")
        events = cancellation.get("events")
        if events != [{"type": "terminal", "status": "cancelled"}]:
            raise RuntimeError(
                "remote session cancellation did not return a terminal cancelled event"
            )
        if cancellation.get("stopReason") != "terminal":
            raise RuntimeError(
                "remote session cancellation did not stop at its terminal event"
            )
        _advance_source_stage(
            state_file,
            stage,
            cancellationTerminal={
                "state": "cancelled",
                "events": events,
                "stopReason": "terminal",
            },
        )
        return "final", _assistant("remote-agent pairing E2E complete")
    raise RuntimeError(
        "source Responses request did not match pairing E2E state machine"
    )


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
            if self.role == "source":
                step, events = _source_events(request, self.state_file)
            else:
                step, events = self._target_events(request)
        except RuntimeError as error:
            self.request_log.add("mockError", error=str(error))
            self.send_error(503, str(error))
            return
        self.request_log.add(
            "responses",
            role=self.role,
            step=step,
            callIds=sorted(_call_outputs(request)),
        )
        if step == "start" and self.role == "source":
            self.request_log.add(
                "grantObserved",
                role=self.role,
                hostId=_managed_host_id(self.state_file),
                scopes=["sessionRead", "sessionWrite", "cancellation"],
            )
        if step == "wait-send":
            self.request_log.add("progressWaitObserved", role=self.role)
        if step == "background":
            self.request_log.add(
                "backgroundResponse",
                role=self.role,
                sourceStage="complete",
            )
        if step == "target-held":
            self._send_target_held_response()
            return
        self._send_events(events)

    def _target_events(
        self, request: dict[str, Any]
    ) -> tuple[str, list[dict[str, object]]]:
        current_user_text: str | None = None
        input_items = request.get("input")
        if isinstance(input_items, list):
            for item in reversed(input_items[-MAX_EVENTS:]):
                if not isinstance(item, dict) or item.get("role") != "user":
                    continue
                content = item.get("content")
                if not isinstance(content, list):
                    continue
                for content_item in reversed(content[-MAX_EVENTS:]):
                    if (
                        isinstance(content_item, dict)
                        and content_item.get("type") == "input_text"
                        and isinstance(content_item.get("text"), str)
                    ):
                        current_user_text = content_item["text"]
                        break
                if current_user_text is not None:
                    break
        if current_user_text is not None and TARGET_ITEMS_MARKER in current_user_text:
            outputs = _call_outputs(request)
            if "items-patch" in outputs:
                return "target-items-final", _assistant(TARGET_ITEMS_DONE)
            if "items-shell" in outputs:
                patch = (
                    "apply_patch <<'EOF'\n*** Begin Patch\n"
                    f"*** Add File: {TARGET_ITEMS_FILE}\n+items e2e\n*** End Patch\nEOF"
                )
                return "target-items-patch", _shell_call("items-patch", patch)
            return "target-items-shell", _shell_call(
                "items-shell",
                f"echo {TARGET_ITEMS_COMMAND_OUTPUT}",
                TARGET_ITEMS_REASONING,
            )
        if current_user_text is not None and RESULT_MARKER in current_user_text:
            return "target-result", _assistant("remote-agent-e2e-tmp-entry")
        if current_user_text is not None and TUI_FOLLOW_UP_MARKER in current_user_text:
            return "target-tui-follow-up", _assistant(TUI_FOLLOW_UP_OUTPUT_MARKER)
        if current_user_text is None or TARGET_MARKER not in current_user_text:
            return "target-final", _assistant("target idle")
        return "target-held", []

    def _send_target_held_response(self) -> None:
        response_id = "pairing-e2e-target-live"
        events = [
            _event("response.created", response={"id": response_id}),
            _event(
                "response.output_item.done",
                item={
                    "type": "message",
                    "role": "assistant",
                    "id": f"{response_id}-message",
                    "status": "completed",
                    "content": [
                        {"type": "output_text", "text": TARGET_LIVE_OUTPUT_MARKER}
                    ],
                },
            ),
        ]
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.end_headers()
        try:
            for event in events:
                self.wfile.write(
                    f"event: {event['type']}\ndata: {_json(event)}\n\n".encode()
                )
                self.wfile.flush()
        except BrokenPipeError:
            return
        self.request_log.add("targetHeld", marker=True)
        interruption_key = (
            "targetTuiInterrupted"
            if _state_read(self.state_file).get("targetInterrupted") is True
            else "targetInterrupted"
        )
        deadline = time.monotonic() + 90.0
        while time.monotonic() < deadline:
            held_state = _state_read(self.state_file)
            if held_state.get("targetSteerRelease") is True:
                _state_update(self.state_file, targetSteerRelease=False)
                self.request_log.add("targetSteerReleased", marker=True)
                completed = _completed(response_id)
                self.wfile.write(
                    f"event: {completed['type']}\ndata: {_json(completed)}\n\n".encode()
                )
                self.wfile.flush()
                return
            if held_state.get(interruption_key) is True:
                self.request_log.add("targetInterruptionRecorded", marker=True)
                return
            time.sleep(STATE_POLL_SECONDS)
        raise RuntimeError("target turn was not interrupted")

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
        self.source = SessionScriptClient.connect_unix_socket(
            args.source_socket, args.timeout
        )
        self.target = SessionScriptClient.connect_unix_socket(
            args.target_socket, args.timeout
        )
        self.source.set_notification_handler(
            lambda message: self._notification("source", message)
        )
        self.target.set_notification_handler(
            lambda message: self._notification("target", message)
        )
        self.source.set_server_request_handler(
            lambda message: self._server_request("source", message)
        )
        self.target.set_server_request_handler(
            lambda message: self._server_request("target", message)
        )
        self.source_complete = False
        self.target_interrupted = False
        self.completed_task_result_recorded = False
        self.progress_recorded = False
        self.remote_activities: list[dict[str, str | None]] = []
        self.approvals = 0
        self.reader_errors: queue.Queue[tuple[str, BaseException]] = queue.Queue(
            maxsize=2
        )
        self.reader_threads: list[threading.Thread] = []
        self.closing = threading.Event()

    def _server_request(self, role: str, message: dict[str, Any]) -> dict[str, Any]:
        method = message.get("method")
        self.recorder.add("serverRequest", role=role, method=method)
        if method != "item/extensionInteraction/request":
            raise RuntimeError(f"unexpected {role} server request: {method!r}")
        params = message.get("params")
        if not isinstance(params, dict):
            raise RuntimeError("extension approval request omitted params")
        surface = params.get("surface")
        actions = surface.get("actions") if isinstance(surface, dict) else None
        if not isinstance(actions, list):
            raise RuntimeError("extension approval request omitted actions")
        action = next(
            (
                item
                for item in actions
                if isinstance(item, dict)
                and item.get("id") in {"approve-session", "approve", "accept"}
            ),
            None,
        )
        if action is None:
            raise RuntimeError("extension approval request has no accepted action")
        self.approvals += 1
        self.recorder.add("extensionApproval", role=role, actionId=action["id"])
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
        method = message.get("method")
        if method == "remoteSession/updated":
            self.recorder.add(
                "remoteSessionUpdate",
                role=role,
                params=message.get("params"),
            )
            if role == "source":
                self._record_remote_session(message.get("params"))
            return
        if method != "turn/completed":
            return
        params = message.get("params")
        turn = params.get("turn") if isinstance(params, dict) else None
        status = turn.get("status") if isinstance(turn, dict) else None
        self.recorder.add("turnCompleted", role=role, status=status)
        if role == "source" and status == "completed":
            self.source_complete = True
        if role == "target" and status == "interrupted":
            self.target_interrupted = True
            _state_update(self.args.state_file, targetInterrupted=True)
            self.recorder.add("targetInterrupted", status=status)

    def _record_remote_session(self, params: Any) -> None:
        session = params.get("remoteSession") if isinstance(params, dict) else None
        if not isinstance(session, dict):
            raise RuntimeError("remote session notification was invalid")
        status = session.get("status")
        remote_session_id = session.get("remoteSessionId")
        summary = session.get("activitySummary")
        if summary == ACTIVITY_SUMMARY_TEXT:
            _state_update(self.args.state_file, remoteSummaryGenerated=True)
        if (
            not isinstance(status, str)
            or not isinstance(remote_session_id, str)
            or not remote_session_id
            or (
                summary is not None
                and (
                    not isinstance(summary, str)
                    or len(summary.encode()) > MAX_ACTIVITY_BYTES
                )
            )
        ):
            raise RuntimeError("remote session notification was invalid or unbounded")
        self.remote_activities.append(
            {
                "status": status,
                "remoteSessionId": remote_session_id,
                "activitySummary": summary,
            }
        )

    def run(self) -> None:
        _state_update(self.args.state_file, requireGeneratedSummary=True)
        self.source.initialize("pairing-e2e-source", "Pairing E2E source", "0.1.0")
        self.target.initialize("pairing-e2e-target", "Pairing E2E target", "0.1.0")
        response = self.source.request("thread/start", {"cwd": self.args.cwd})
        thread = response.get("thread")
        thread_id = thread.get("id") if isinstance(thread, dict) else None
        if not isinstance(thread_id, str):
            raise RuntimeError("source thread/start omitted id")
        self.args.ready_file.write_text(
            _json({"sourceThreadId": thread_id}) + "\n", encoding="utf-8"
        )
        self.recorder.add("sourceReady", threadId=thread_id)
        self.source.request(
            "turn/start",
            {"threadId": thread_id, "input": [{"type": "text", "text": SOURCE_MARKER}]},
        )
        self._start_reader("source", self.source)
        target_thread_id = self._wait_for_state("targetThreadId")
        if not isinstance(target_thread_id, str):
            raise RuntimeError("target thread id is invalid")
        self._record_completed_task_result_before_observer()
        self.target.request("thread/resume", {"threadId": target_thread_id})
        self._start_reader("target", self.target)
        _state_update(self.args.state_file, targetObserverSubscribed=True)
        self.recorder.add("targetObserverSubscribed", threadId=target_thread_id)
        self._wait()
        if not self.target_interrupted:
            raise RuntimeError("target interruption was not observed")
        if not self.source_complete:
            raise RuntimeError("source did not receive its terminal result")
        self._assert_remote_activity_lifecycle()
        self.recorder.add(
            "controllerPassed", approvals=self.approvals, targetInterrupted=True
        )

    def _assert_remote_activity_lifecycle(self) -> None:
        cancelled_index = next(
            (
                index
                for index in range(len(self.remote_activities) - 1, -1, -1)
                if self.remote_activities[index]["status"] == "cancelled"
            ),
            None,
        )
        if cancelled_index is None:
            raise RuntimeError("source did not receive cancelled remote session update")
        remote_session_id = self.remote_activities[cancelled_index]["remoteSessionId"]
        lifecycle = [
            activity
            for activity in self.remote_activities[: cancelled_index + 1]
            if activity["remoteSessionId"] == remote_session_id
        ]
        if not any(
            activity["status"] == "running"
            and activity["activitySummary"] == "Remote session is running."
            for activity in lifecycle
        ):
            raise RuntimeError("remote session did not publish the running summary")
        self.recorder.add(
            "remoteActivityLifecycle",
            remoteSessionId=remote_session_id,
            statuses=[activity["status"] for activity in lifecycle],
        )

    def _record_completed_task_result_before_observer(self) -> None:
        deadline = time.monotonic() + self.args.wait_timeout
        while time.monotonic() < deadline:
            completed_task_result = _state_read(self.args.state_file).get(
                "completedTaskResult"
            )
            if isinstance(completed_task_result, dict):
                self.completed_task_result_recorded = True
                self.recorder.add("completedTaskResult", **completed_task_result)
                return
            if completed_task_result is not None:
                raise RuntimeError("completed peer task result evidence is invalid")
            self._wait_for_reader_activity()
        raise RuntimeError("timed out waiting for completed peer task result")

    def _wait(self) -> None:
        deadline = time.monotonic() + self.args.wait_timeout
        while time.monotonic() < deadline:
            state = _state_read(self.args.state_file)
            completed_task_result = state.get("completedTaskResult")
            if not self.completed_task_result_recorded:
                if isinstance(completed_task_result, dict):
                    self.completed_task_result_recorded = True
                    self.recorder.add(
                        "completedTaskResult",
                        **completed_task_result,
                    )
                elif completed_task_result is not None:
                    raise RuntimeError("completed peer task result evidence is invalid")
            progress_events = state.get("progressEvents")
            if (
                not self.progress_recorded
                and isinstance(progress_events, list)
                and any(
                    isinstance(event, dict)
                    and event.get("type") == "progress"
                    and event.get("status") == "running"
                    and isinstance(event.get("activitySummary"), str)
                    and event["activitySummary"]
                    for event in progress_events
                )
            ):
                self.progress_recorded = True
                self.recorder.add("remoteWaitProgress", events=progress_events)
            if (
                self.source_complete
                and self.target_interrupted
                and self.completed_task_result_recorded
                and self.progress_recorded
            ):
                return
            self._wait_for_reader_activity()
        raise RuntimeError("timed out waiting for pairing E2E completion")

    def _wait_for_state(self, key: str) -> Any:
        deadline = time.monotonic() + self.args.wait_timeout
        while time.monotonic() < deadline:
            value = _state_read(self.args.state_file).get(key)
            if value is not None:
                return value
            self._wait_for_reader_activity()
        raise RuntimeError(f"timed out waiting for E2E state {key}")

    def _start_reader(self, role: str, client: SessionScriptClient) -> None:
        client._transport.socket.settimeout(None)
        reader = threading.Thread(
            target=self._read_messages,
            args=(role, client),
            daemon=True,
            name=f"pairing-e2e-{role}-reader",
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
        time.sleep(STATE_POLL_SECONDS)
        self._raise_reader_error()

    def _raise_reader_error(self) -> None:
        try:
            role, error = self.reader_errors.get_nowait()
        except queue.Empty:
            return
        raise RuntimeError(f"{role} controller reader failed: {error}") from error

    def close(self) -> None:
        self.closing.set()
        self.source.close()
        self.target.close()
        for reader in self.reader_threads:
            reader.join()


def _run_mock(args: argparse.Namespace) -> int:
    args.request_log.touch()
    handler = type("PairingResponsesHandler", (ResponsesHandler,), {})
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
        "mock", help="serve a local deterministic Responses mock"
    )
    mock.add_argument("--role", choices=("source", "target"), required=True)
    mock.add_argument("--port-file", type=Path, required=True)
    mock.add_argument("--request-log", type=Path, required=True)
    mock.add_argument("--state-file", type=Path, required=True)
    mock.set_defaults(run=_run_mock)
    controller = commands.add_parser(
        "controller", help="drive and observe both app servers"
    )
    controller.add_argument("--source-socket", type=Path, required=True)
    controller.add_argument("--target-socket", type=Path, required=True)
    controller.add_argument("--cwd", required=True)
    controller.add_argument("--ready-file", type=Path, required=True)
    controller.add_argument("--evidence-file", type=Path, required=True)
    controller.add_argument("--state-file", type=Path, required=True)
    controller.add_argument("--timeout", type=float, default=0.25)
    controller.add_argument("--wait-timeout", type=float, default=90.0)
    controller.set_defaults(run=_run_controller)
    return parser


def main() -> int:
    args = _parser().parse_args()
    try:
        return args.run(args)
    except (OSError, RpcError, RuntimeError, ValueError) as error:
        print(f"remote-agent pairing E2E failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
