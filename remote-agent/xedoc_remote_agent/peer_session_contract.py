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
        "session/send",
        "session/status",
        "session/wait",
        "session/cancel",
        "session/detach",
    }
)
MAX_RELAY_EVENTS = 32
_MAX_TIMEOUT_PRECISION = 3
_SESSION_FIELDS: dict[str, tuple[set[str], set[str]]] = {
    "session/start": ({"workspaceId", "relativePath"}, {"workspaceId"}),
    "session/resume": ({"threadId"}, {"threadId"}),
    "session/attach": ({"threadId"}, {"threadId"}),
    "session/send": ({"threadId", "message"}, {"threadId", "message"}),
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
    elif operation == "session/send":
        identifier(value["threadId"])
        if not isinstance(value["message"], str) or not value["message"]:
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
        "session/cancel",
        "session/detach",
    }:
        if set(result) != {"operationId"}:
            raise BrokerError.invalid_request()
        identifier(result["operationId"])
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
        if (
            not isinstance(event, Mapping)
            or set(event) != {"type", "status"}
            or event["type"] not in {"progress", "terminal"}
            or event["status"]
            not in {"running", "completed", "failed", "cancelled", "expired"}
        ):
            raise BrokerError.invalid_request()
    error = value.get("error")
    if error is not None and (
        not isinstance(error, Mapping)
        or set(error) != {"code", "message", "retryable"}
        or not isinstance(error.get("code"), str)
        or not isinstance(error.get("message"), str)
        or not isinstance(error.get("retryable"), bool)
    ):
        raise BrokerError.invalid_request()
    if "result" in value and not isinstance(value["result"], Mapping):
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
