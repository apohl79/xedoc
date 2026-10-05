"""Fixed, bounded wire contracts for Stage 5 peer session operations."""

from __future__ import annotations

import json
from typing import Any, Mapping

from .errors import BrokerError
from .models import MAX_ID_LENGTH, OperationState


SESSION_OPERATIONS = frozenset(
    {
        "session/start",
        "session/resume",
        "session/attach",
        "session/read",
        "session/send",
        "session/steer",
        "session/status",
        "session/wait",
        "session/cancel",
        "session/detach",
    }
)
MAX_RELAY_EVENTS = 32
MAX_OUTPUT_TEXT_BYTES = 32 * 1024
MAX_ACTIVITY_DELTA_BYTES = 4 * 1024
MAX_ACTIVITY_SUMMARY_LENGTH = 64
MAX_RELAY_READ_LIMIT = MAX_RELAY_EVENTS
_MAX_TIMEOUT_PRECISION = 3
_SESSION_FIELDS: dict[str, tuple[set[str], set[str]]] = {
    "session/start": ({"workspaceId", "relativePath"}, {"workspaceId"}),
    "session/resume": ({"threadId"}, {"threadId"}),
    "session/attach": ({"threadId"}, {"threadId"}),
    "session/read": ({"threadId", "cursor", "limit"}, {"threadId"}),
    "session/send": ({"threadId", "message"}, {"threadId", "message"}),
    "session/steer": (
        {"threadId", "turnId", "message"},
        {"threadId", "turnId", "message"},
    ),
    "session/status": ({"threadId"}, {"threadId"}),
    "session/wait": ({"operationId", "timeoutSeconds"}, {"operationId", "timeoutSeconds"}),
    "session/cancel": ({"threadId", "turnId"}, {"threadId", "turnId"}),
    "session/detach": ({"threadId"}, {"threadId"}),
}
_SENSITIVE_KEYS = {
    "cwd",
    "path",
    "root",
    "rollout",
    "notification",
    "notifications",
    "appServer",
}


def validate_session_params(operation: str, params: Mapping[str, Any]) -> dict[str, Any]:
    """Reject every peer payload outside the fixed Stage 5 operation contract."""

    if operation not in SESSION_OPERATIONS or not isinstance(params, Mapping):
        raise BrokerError.invalid_request()
    allowed, required = _SESSION_FIELDS[operation]
    if set(params) - allowed or not required.issubset(params):
        raise BrokerError.invalid_request()
    value = dict(params)
    if operation == "session/start":
        identifier(value["workspaceId"])
        relative_path = value.get("relativePath")
        if relative_path is not None and not isinstance(relative_path, str):
            raise BrokerError.invalid_request()
    elif operation in {
        "session/resume",
        "session/attach",
        "session/status",
        "session/detach",
    }:
        identifier(value["threadId"])
    elif operation in {"session/send", "session/steer"}:
        identifier(value["threadId"])
        if operation == "session/steer":
            identifier(value["turnId"])
        if not isinstance(value["message"], str) or not value["message"]:
            raise BrokerError.invalid_request()
    elif operation == "session/read":
        identifier(value["threadId"])
        cursor = value.get("cursor")
        if cursor is not None:
            identifier(cursor)
        limit = value.get("limit")
        if (
            limit is not None
            and (
                not isinstance(limit, int)
                or isinstance(limit, bool)
                or not 1 <= limit <= MAX_RELAY_READ_LIMIT
            )
        ):
            raise BrokerError.invalid_request()
    elif operation == "session/wait":
        identifier(value["operationId"])
        timeout = value["timeoutSeconds"]
        if (
            not isinstance(timeout, (int, float))
            or isinstance(timeout, bool)
            or timeout < 0
            or round(float(timeout), _MAX_TIMEOUT_PRECISION) != float(timeout)
        ):
            raise BrokerError.invalid_request()
    else:
        identifier(value["threadId"])
        identifier(value["turnId"])
    return value


def validate_session_result(
    operation: str, value: Mapping[str, Any], max_result_bytes: int
) -> dict[str, Any]:
    """Validate a peer session projection before exposing it through local IPC."""

    if operation not in SESSION_OPERATIONS or not isinstance(value, Mapping):
        raise BrokerError.invalid_request()
    result = dict(value)
    if encoded_size(result) > max_result_bytes:
        raise BrokerError.limit_exceeded()
    _reject_sensitive(result)
    if operation in {
        "session/start",
        "session/resume",
        "session/attach",
        "session/send",
        "session/steer",
        "session/cancel",
        "session/detach",
    }:
        if set(result) != {"operationId"}:
            raise BrokerError.invalid_request()
        identifier(result["operationId"])
        return result
    if operation == "session/read":
        _validate_read_result(result)
        return result
    if operation == "session/status":
        allowed = {"threadId", "workspaceId", "isRunning", "status", "activeTurnId"}
        required = {"threadId", "workspaceId", "isRunning", "status"}
        if set(result) - allowed or not required.issubset(result):
            raise BrokerError.invalid_request()
        identifier(result["threadId"])
        identifier(result["workspaceId"])
        if not isinstance(result["isRunning"], bool) or not isinstance(result["status"], str):
            raise BrokerError.invalid_request()
        if len(result["status"]) > 64:
            raise BrokerError.invalid_request()
        if "activeTurnId" in result:
            identifier(result["activeTurnId"])
        return result
    _validate_wait_result(result)
    return result


def identifier(value: Any) -> str:
    """Validate an opaque peer-visible identifier."""

    if (
        not isinstance(value, str)
        or not value
        or len(value) > MAX_ID_LENGTH
        or any(character.isspace() or ord(character) < 33 for character in value)
    ):
        raise BrokerError.invalid_request()
    return value


def encoded_size(value: Any) -> int:
    """Return the canonical response size without allowing non-finite values."""

    try:
        return len(
            json.dumps(
                value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
            ).encode("utf-8")
        )
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error


def _validate_wait_result(value: Mapping[str, Any]) -> None:
    allowed = {
        "operationId",
        "operation",
        "state",
        "threadId",
        "turnId",
        "result",
        "error",
        "events",
        "stopReason",
    }
    required = {"operationId", "operation", "state", "events", "stopReason"}
    if set(value) - allowed or not required.issubset(value):
        raise BrokerError.invalid_request()
    identifier(value["operationId"])
    if value["operation"] not in SESSION_OPERATIONS:
        raise BrokerError.invalid_request()
    if value["state"] not in {state.value for state in OperationState}:
        raise BrokerError.invalid_request()
    if value["stopReason"] not in {"terminal", "timeout", "itemLimit", "byteLimit"}:
        raise BrokerError.invalid_request()
    for key in ("threadId", "turnId"):
        if key in value:
            identifier(value[key])
    events = value["events"]
    if not isinstance(events, list) or len(events) > MAX_RELAY_EVENTS:
        raise BrokerError.invalid_request()
    for event in events:
        _validate_wait_event(event)
    error = value.get("error")
    if error is not None and (
        not isinstance(error, Mapping)
        or set(error) != {"code", "message", "retryable"}
        or not isinstance(error.get("code"), str)
        or not isinstance(error.get("message"), str)
        or not isinstance(error.get("retryable"), bool)
    ):
        raise BrokerError.invalid_request()
    result = value.get("result")
    if result is not None:
        if not isinstance(result, Mapping):
            raise BrokerError.invalid_request()
        _validate_wait_operation_result(value["operation"], result)


def _validate_read_result(value: Mapping[str, Any]) -> None:
    allowed = {
        "threadId",
        "workspaceId",
        "isRunning",
        "status",
        "activeTurnId",
        "events",
        "nextCursor",
    }
    required = {
        "threadId",
        "workspaceId",
        "isRunning",
        "status",
        "events",
        "nextCursor",
    }
    if set(value) - allowed or not required.issubset(value):
        raise BrokerError.invalid_request()
    identifier(value["threadId"])
    identifier(value["workspaceId"])
    if not isinstance(value["isRunning"], bool):
        raise BrokerError.invalid_request()
    status = value["status"]
    if not isinstance(status, str) or len(status) > 64:
        raise BrokerError.invalid_request()
    if "activeTurnId" in value:
        identifier(value["activeTurnId"])
    next_cursor = value["nextCursor"]
    if next_cursor is not None:
        identifier(next_cursor)
    events = value["events"]
    if not isinstance(events, list) or len(events) > MAX_RELAY_READ_LIMIT:
        raise BrokerError.invalid_request()
    for event in events:
        _validate_read_event(event)


def _validate_read_event(event: Any) -> None:
    if not isinstance(event, Mapping):
        raise BrokerError.invalid_request()
    event = dict(event)
    if set(event) != {"cursor", "type", "turnId", "text", "isDelta"}:
        raise BrokerError.invalid_request()
    identifier(event["cursor"])
    if event["type"] not in {
        "activity",
        "userMessage",
        "agentMessage",
        "commandExecution",
    }:
        raise BrokerError.invalid_request()
    identifier(event["turnId"])
    text = event["text"]
    if (
        not isinstance(text, str)
        or not text
        or len(text.encode("utf-8")) > MAX_ACTIVITY_DELTA_BYTES
        or not isinstance(event["isDelta"], bool)
    ):
        raise BrokerError.invalid_request()


def _validate_wait_event(event: Any) -> None:
    if not isinstance(event, Mapping):
        raise BrokerError.invalid_request()
    event = dict(event)
    event_type = event.get("type")
    status = event.get("status")
    if (
        event_type not in {"progress", "terminal"}
        or status not in {"running", "completed", "failed", "cancelled", "expired"}
    ):
        raise BrokerError.invalid_request()
    if event_type == "terminal":
        if set(event) != {"type", "status"}:
            raise BrokerError.invalid_request()
        return
    allowed = {
        "type",
        "status",
        "activitySummary",
        "outputDelta",
        "outputCursor",
    }
    if set(event) - allowed or set(event) < {"type", "status", "activitySummary"}:
        raise BrokerError.invalid_request()
    summary = event["activitySummary"]
    if not isinstance(summary, str) or len(summary) > MAX_ACTIVITY_SUMMARY_LENGTH:
        raise BrokerError.invalid_request()
    output_delta = event.get("outputDelta")
    output_cursor = event.get("outputCursor")
    if (output_delta is None) != (output_cursor is None):
        raise BrokerError.invalid_request()
    if output_delta is not None and (
        not isinstance(output_delta, str)
        or len(output_delta.encode("utf-8")) > MAX_ACTIVITY_DELTA_BYTES
    ):
        raise BrokerError.invalid_request()
    if output_cursor is not None:
        identifier(output_cursor)


def _validate_wait_operation_result(
    operation: str, result: Mapping[str, Any]
) -> None:
    result = dict(result)
    result_fields = {
        "session/start": {"threadId", "workspaceId"},
        "session/resume": {"threadId", "workspaceId"},
        "session/attach": {"threadId", "workspaceId"},
        "session/send": {"threadId", "turnId", "status", "outputText"},
        "session/steer": {"threadId", "turnId"},
        "session/cancel": {"threadId", "turnId"},
        "session/detach": {"threadId"},
    }
    allowed = result_fields.get(operation)
    if allowed is None or set(result) - allowed:
        raise BrokerError.invalid_request()
    for key in ("threadId", "turnId", "workspaceId"):
        if key in result:
            identifier(result[key])
    if operation == "session/send":
        if "status" in result and result["status"] not in {
            "completed",
            "failed",
            "cancelled",
        }:
            raise BrokerError.invalid_request()
        output_text = result.get("outputText")
        if output_text is not None and (
            not isinstance(output_text, str)
            or len(output_text.encode("utf-8")) > MAX_OUTPUT_TEXT_BYTES
        ):
            raise BrokerError.invalid_request()


def _reject_sensitive(value: Any) -> None:
    if isinstance(value, Mapping):
        for key, item in value.items():
            if key in _SENSITIVE_KEYS:
                raise BrokerError.invalid_request()
            _reject_sensitive(item)
    elif isinstance(value, list):
        for item in value:
            _reject_sensitive(item)
