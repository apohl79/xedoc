"""Thread-scoped built-in remote-agent extension child."""

from __future__ import annotations

import json
import os
import queue
import select
import sys
import threading
from typing import Any, Callable, Mapping, Sequence

from .errors import BrokerError
from .ipc import PROTOCOL
from .ipc import PROTOCOL_VERSION
from .ipc import LocalIpcClient
from .peer_session_items import entry_key, take_within_budget, without_items
from .peer_sessions import entries_text
from .workspaces import controller_identifier
from .workspaces import load_bootstrap_descriptor


_VERSION = "0.1.0"
_MAX_ARGUMENT_BYTES = 64 * 1024
_MAX_RESULT_BYTES = 256 * 1024
_MAX_IDENTIFIER_LENGTH = 128
_MAX_WAIT_SECONDS = 3_600
_WAIT_TIMEOUT_BUFFER_SECONDS = 5.0
_SESSION_START_TIMEOUT_SECONDS = 30.0
_REMOTE_SESSION_WATCH_POLL_SECONDS = 1.0
_REMOTE_SESSION_UPDATE_BYTES = 4_096
_MAX_FORWARDED_ITEMS = 4_096
_REMOTE_NAMESPACE = "remote"
_REMOTE_SESSION_CONTROL_METHOD = "remoteSession/control"
_REMOTE_SESSION_REGISTER_METHOD = "script/remoteSessionRegister"
_REMOTE_SESSION_UPDATE_METHOD = "script/remoteSessionUpdate"
_MAX_REMOTE_SESSION_READ_LIMIT = 32
_LOCAL_METHODS = {
    "hosts_list": "host/list",
    "hosts_discover": "host/discover",
    "host_pair": "host/pair",
    "pairing_requests": "pairing/list",
    "pairing_approve": "pairing/approve",
    "pairing_reject": "pairing/reject",
    "host_grants_list": "host/grants/list",
    "host_grant_set": "host/grants/set",
    "host_suspend": "host/suspend",
    "host_revoke": "host/revoke",
    "host_remove": "host/remove",
    "host_rotate": "host/rotate",
    "workspaces_list": "workspace/list",
    "sessions_list": "session/list",
    "sessions_search": "session/search",
    "session_start": "session/start",
    "session_resume": "session/resume",
    "session_attach": "session/attach",
    "session_read": "session/read",
    "session_send": "session/send",
    "session_steer": "session/steer",
    "session_message": "session/message",
    "session_status": "session/status",
    "session_wait": "session/wait",
    "session_cancel": "session/cancel",
    "session_detach": "session/detach",
    "request_review": "request/review",
    "request_approve": "request/approve",
    "request_reject": "request/reject",
}
_REMOTE_SESSION_CONTROL_BASE_FIELDS = {"action", "hostId", "threadId"}
_REMOTE_SESSION_CONTROL_NULLABLE_FIELDS = {
    "message",
    "expectedTurnId",
    "cursor",
    "limit",
}
_REMOTE_SESSION_CONTROL_FIELDS: dict[str, set[str]] = {
    "attach": set(),
    "read": {"cursor", "limit"},
    "send": {"message"},
    "steer": {"expectedTurnId", "message"},
    "cancel": {"expectedTurnId"},
    "detach": set(),
}
_REQUEST_FIELDS = {
    "threadId",
    "turnId",
    "callId",
    "namespace",
    "tool",
    "arguments",
    "extensionId",
}


def _load_session_script_sdk():
    """Load the SDK only from the bundled payload, never from the checkout."""

    try:
        from scripts import session_script_sdk
    except ImportError as error:
        raise RuntimeError("bundled session-script SDK is unavailable") from error
    return session_script_sdk


def _bounded_identifier(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > _MAX_IDENTIFIER_LENGTH
    ):
        raise BrokerError.invalid_request()
    return value


def _encoded(value: Any, limit: int) -> bytes:
    try:
        encoded = json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    if not encoded or len(encoded) > limit:
        raise BrokerError.limit_exceeded()
    return encoded


def _response(payload: Mapping[str, Any], success: bool) -> dict[str, Any]:
    encoded = _encoded(payload, _MAX_RESULT_BYTES)
    return {
        "contentItems": [
            {"type": "inputText", "text": encoded.decode("utf-8")}
        ],
        "success": success,
    }


def _failed(error: BrokerError) -> dict[str, Any]:
    return _response(
        {
            "status": "failed",
            "error": {
                "code": error.code.value,
                "message": error.message,
                "retryable": error.retryable,
            },
        },
        False,
    )


def _remote_session_control_failure(error: BrokerError) -> dict[str, Any]:
    return {
        "success": False,
        "result": {
            "error": {
                "code": error.code.value,
                "message": error.message,
                "retryable": error.retryable,
            },
        },
    }


class RemoteAgentExtension:
    """Validate dynamic tool calls and dispatch only fixed local broker methods."""

    def __init__(
        self,
        broker: LocalIpcClient,
        *,
        thread_id: str,
        extension_id: str,
        source_lease: str | None = None,
        app_server_request: (
            Callable[[str, Mapping[str, Any]], Mapping[str, Any]] | None
        ) = None,
    ) -> None:
        self._broker = broker
        self._thread_id = _bounded_identifier(thread_id)
        self._extension_id = _bounded_identifier(extension_id)
        self._source_lease = (
            _bounded_identifier(source_lease) if source_lease is not None else None
        )
        self._app_server_request = app_server_request
        self._script_registration_id: str | None = None
        self._remote_sessions: dict[tuple[str, str], str] = {}
        self._remote_hostnames: dict[str, str] = {}
        self._operation_sessions: dict[str, tuple[str, str | None, str | None]] = {}
        self._active_operations: dict[tuple[str, str], str] = {}
        # The registry only accepts a terminal update that names the active turn.
        self._remote_session_turns: dict[str, str] = {}
        # Items already sent to the app-server per session, so the final
        # transcript projection only adds what the live stream missed.
        self._forwarded_items: dict[str, set[tuple[str, str, str]]] = {}
        self._pending_remote_session_updates: queue.SimpleQueue[dict[str, Any]] = (
            queue.SimpleQueue()
        )

    def set_source_lease(self, source_lease: str) -> None:
        """Attach the broker-issued lease after host registration succeeds."""

        self._source_lease = _bounded_identifier(source_lease)

    def set_script_registration(self, registration_id: str) -> None:
        """Bind trusted remote-session updates to this script's registered root."""

        self._script_registration_id = _bounded_identifier(registration_id)

    def handle_request(self, request: dict[str, Any]) -> dict[str, Any]:
        try:
            if request.get("method") == _REMOTE_SESSION_CONTROL_METHOD:
                return self._handle_remote_session_control(request)
            return self._handle_request(request)
        except BrokerError as error:
            if request.get("method") == _REMOTE_SESSION_CONTROL_METHOD:
                return _remote_session_control_failure(error)
            return _failed(error)
        except BaseException:
            if request.get("method") == _REMOTE_SESSION_CONTROL_METHOD:
                return _remote_session_control_failure(BrokerError.internal())
            return _failed(BrokerError.internal())

    def _handle_request(self, request: Mapping[str, Any]) -> dict[str, Any]:
        if request.get("method") != "item/tool/call":
            raise BrokerError.invalid_request()
        params = request.get("params")
        if not isinstance(params, Mapping) or set(params) - _REQUEST_FIELDS:
            raise BrokerError.invalid_request()
        required = {
            "threadId",
            "turnId",
            "callId",
            "namespace",
            "tool",
            "arguments",
        }
        if not required.issubset(params):
            raise BrokerError.invalid_request()
        if _bounded_identifier(params["threadId"]) != self._thread_id:
            raise BrokerError.unauthorized()
        extension_id = params.get("extensionId")
        if extension_id is not None and (
            _bounded_identifier(extension_id) != self._extension_id
        ):
            raise BrokerError.unauthorized()
        _bounded_identifier(params["turnId"])
        _bounded_identifier(params["callId"])

        tool = _tool_name(params.get("namespace"), params["tool"])
        arguments = params["arguments"]
        if not isinstance(arguments, Mapping):
            raise BrokerError.invalid_request()
        _encoded(arguments, _MAX_ARGUMENT_BYTES)

        method = _LOCAL_METHODS.get(tool)
        if method is not None:
            source_lease = None
            if method == "session/message":
                source_lease = self._source_lease
                if source_lease is None:
                    raise BrokerError.unavailable()
            result = self._broker.call(
                method,
                arguments,
                timeout_seconds=_broker_timeout(tool, arguments),
                extension_lease=source_lease,
            )
            self._remember_host_metadata(tool, result)
            self._register_tool_remote_session(tool, arguments, result)
            self._update_tool_remote_session(tool, arguments, result)
            return _response(
                {"status": "ok", "result": _model_visible_result(tool, result)}, True
            )
        raise BrokerError.not_found()

    def _handle_remote_session_control(
        self, request: Mapping[str, Any]
    ) -> dict[str, Any]:
        params = request.get("params")
        if not isinstance(params, Mapping):
            raise BrokerError.invalid_request()
        action = params.get("action")
        if action not in _REMOTE_SESSION_CONTROL_FIELDS:
            raise BrokerError.invalid_request()
        allowed_values = _REMOTE_SESSION_CONTROL_FIELDS[action]
        allowed = (
            _REMOTE_SESSION_CONTROL_BASE_FIELDS
            | _REMOTE_SESSION_CONTROL_NULLABLE_FIELDS
        )
        required = _REMOTE_SESSION_CONTROL_BASE_FIELDS | (
            allowed_values - _REMOTE_SESSION_CONTROL_NULLABLE_FIELDS
        )
        if set(params) - allowed or not required.issubset(params):
            raise BrokerError.invalid_request()
        if any(
            params.get(field) is not None
            for field in _REMOTE_SESSION_CONTROL_NULLABLE_FIELDS - allowed_values
        ):
            raise BrokerError.invalid_request()
        host_id = _bounded_identifier(params["hostId"])
        thread_id = _bounded_identifier(params["threadId"])
        broker_params: dict[str, Any] = {"hostId": host_id, "threadId": thread_id}
        if action == "read":
            cursor = params.get("cursor")
            if cursor is not None:
                broker_params["cursor"] = _bounded_identifier(cursor)
            limit = params.get("limit")
            if limit is not None:
                if (
                    not isinstance(limit, int)
                    or isinstance(limit, bool)
                    or not 1 <= limit <= _MAX_REMOTE_SESSION_READ_LIMIT
                ):
                    raise BrokerError.invalid_request()
                broker_params["limit"] = limit
        elif action in {"send", "steer"}:
            message = params["message"]
            if not isinstance(message, str) or not message:
                raise BrokerError.invalid_request()
            broker_params["message"] = message
        if action in {"steer", "cancel"}:
            expected_turn_id = params.get("expectedTurnId")
            if expected_turn_id is not None:
                broker_params["turnId"] = _bounded_identifier(expected_turn_id)

        if action == "send":
            self._capture_remote_session_output(host_id, thread_id)
        method = f"session/{action}"
        operation_result = self._broker.call(
            method,
            broker_params,
            timeout_seconds=(
                _SESSION_START_TIMEOUT_SECONDS if action == "attach" else None
            ),
        )
        result = operation_result
        if action == "read":
            self._mark_forwarded(host_id, thread_id, result.get("items"))
        if action in {"attach", "steer", "cancel", "detach"}:
            completed = self._await_control_operation(host_id, operation_result)
            _require_completed_control_operation(action, completed)
        if action in {"attach", "steer", "cancel"}:
            result = self._broker.call(
                "session/status",
                {"hostId": host_id, "threadId": thread_id},
            )
        if action in {"send", "steer"}:
            remote_session_id = self._register_remote_session(
                host_id,
                thread_id,
                _optional_hostname(result.get("hostName")),
            )
            operation_id = _optional_identifier(operation_result.get("operationId"))
            if operation_id is not None and remote_session_id is not None:
                self._operation_sessions[operation_id] = (
                    host_id,
                    thread_id,
                    _optional_hostname(result.get("hostName")),
                )
                if action == "send":
                    self._active_operations[(host_id, thread_id)] = operation_id
        if action == "send":
            turn_id = _optional_identifier(result.get("turnId"))
            if turn_id is not None and remote_session_id is not None:
                self._remote_session_turns[remote_session_id] = turn_id
            status = self._broker.call(
                "session/status",
                {"hostId": host_id, "threadId": thread_id},
            )
            self._watch_control_operation(host_id, operation_result)
            result = {
                **status,
                **operation_result,
                "threadId": thread_id,
                "isRunning": True,
                "status": "running",
                "activitySummary": "Remote session is running.",
            }
            if turn_id is not None:
                result["activeTurnId"] = turn_id
        return {"success": True, "result": result}

    def _await_control_operation(
        self, host_id: str, result: Mapping[str, Any]
    ) -> dict[str, Any]:
        return self._wait_for_control_operation(
            host_id, result, _SESSION_START_TIMEOUT_SECONDS
        )

    def _wait_for_control_operation(
        self, host_id: str, result: Mapping[str, Any], timeout_seconds: float
    ) -> dict[str, Any]:
        operation_id = _bounded_identifier(result.get("operationId"))
        return self._broker.call(
            "session/wait",
            {
                "hostId": host_id,
                "operationId": operation_id,
                "timeoutSeconds": timeout_seconds,
            },
            timeout_seconds=timeout_seconds + _WAIT_TIMEOUT_BUFFER_SECONDS,
        )

    def _watch_control_operation(
        self, host_id: str, result: Mapping[str, Any]
    ) -> None:
        operation_id = _optional_identifier(result.get("operationId"))
        if operation_id is None:
            return
        threading.Thread(
            target=self._watch_control_operation_worker,
            args=(host_id, operation_id),
            daemon=True,
            name=f"xedoc-remote-session-{operation_id[:12]}",
        ).start()

    def _watch_control_operation_worker(self, host_id: str, operation_id: str) -> None:
        handle: Mapping[str, Any] = {"operationId": operation_id}
        arguments: Mapping[str, Any] = {
            "hostId": host_id,
            "operationId": operation_id,
        }
        try:
            while True:
                result = self._wait_for_control_operation(
                    host_id, handle, _REMOTE_SESSION_WATCH_POLL_SECONDS
                )
                self._update_tool_remote_session("session_wait", arguments, result)
                if result.get("stopReason") == "terminal":
                    self._project_terminal_output(host_id, operation_id, result)
                    return
        except BrokerError:
            self._mark_remote_session_failed(operation_id)

    def _mark_forwarded(self, host_id: str, thread_id: str, entries: Any) -> None:
        if not isinstance(entries, list):
            return
        remote_session_id = self._remote_sessions.get((host_id, thread_id))
        if remote_session_id is None:
            return
        forwarded = self._forwarded_items.setdefault(remote_session_id, set())
        for entry in entries:
            if len(forwarded) < _MAX_FORWARDED_ITEMS and isinstance(entry, Mapping):
                forwarded.add(entry_key(entry))

    def _capture_remote_session_output(self, host_id: str, thread_id: str) -> None:
        """Record the items already in the transcript so a later turn projects only its own."""

        try:
            for entries in self._read_transcript_pages(host_id, thread_id):
                self._mark_forwarded(host_id, thread_id, entries)
        except BrokerError:
            return

    def _read_transcript_pages(self, host_id: str, thread_id: str):
        cursor: str | None = None
        while True:
            params: dict[str, Any] = {"hostId": host_id, "threadId": thread_id}
            if cursor is not None:
                params["cursor"] = cursor
            result = self._broker.call("session/read", params)
            yield result.get("items")
            cursor = _optional_identifier(result.get("nextCursor"))
            if cursor is None:
                return

    def _project_terminal_output(
        self, host_id: str, operation_id: str, terminal_result: Mapping[str, Any]
    ) -> None:
        identity = self._operation_sessions.get(operation_id)
        if identity is None or identity[1] is None:
            return
        if self._active_operations.get((host_id, identity[1])) != operation_id:
            return
        remote_session_id = self._register_remote_session(*identity)
        if remote_session_id is None:
            return
        terminal_status = terminal_result.get("state")
        if terminal_status not in {"completed", "failed", "cancelled", "expired"}:
            terminal_status = "completed"
        forwarded = self._forwarded_items.setdefault(remote_session_id, set())
        unseen: list[dict[str, Any]] = []
        for entries in self._read_transcript_pages(host_id, identity[1]):
            if isinstance(entries, list):
                unseen.extend(
                    entry
                    for entry in entries
                    if isinstance(entry, dict)
                    and entry["item"].get("type") != "userMessage"
                    and entry_key(entry) not in forwarded
                )
        active_turn_id = self._remote_session_turns.get(remote_session_id)
        chunk_number = 0
        while unseen:
            chunk = take_within_budget(unseen)
            unseen = unseen[len(chunk) :]
            chunk_number += 1
            event: dict[str, Any] = {"status": terminal_status, "items": chunk}
            text = entries_text(chunk)
            if text:
                event["outputDelta"] = text
                event["outputCursor"] = f"operation_{operation_id}_0_{chunk_number}"
            self._publish_remote_session_update(
                remote_session_id, event, active_turn_id
            )

    def _mark_remote_session_failed(self, operation_id: str) -> None:
        identity = self._operation_sessions.get(operation_id)
        if identity is None or identity[1] is None:
            return
        remote_session_id = self._register_remote_session(*identity)
        if remote_session_id is not None:
            self._publish_remote_session_update(
                remote_session_id,
                {
                    "status": "failed",
                    "activitySummary": "Remote session connection failed.",
                },
                self._remote_session_turns.get(remote_session_id),
            )

    def _register_tool_remote_session(
        self, tool: str, arguments: Mapping[str, Any], result: Mapping[str, Any]
    ) -> None:
        host_id = _optional_identifier(arguments.get("hostId"))
        if host_id is None:
            return
        host_name = _optional_hostname(result.get("hostName"))
        thread_id = _optional_identifier(arguments.get("threadId"))
        operation_id = _optional_identifier(result.get("operationId"))
        if tool in {"session_resume", "session_attach", "session_send", "session_steer"}:
            if thread_id is None:
                return
            remote_session_id = self._register_remote_session(
                host_id, thread_id, host_name
            )
            if tool in {"session_send", "session_steer"} and remote_session_id:
                self._publish_remote_session_update(
                    remote_session_id,
                    {
                        "status": "running",
                        "activitySummary": "Remote session is running.",
                    },
                    _optional_identifier(result.get("turnId")),
                )
        if operation_id is not None:
            self._operation_sessions[operation_id] = (host_id, thread_id, host_name)
            if tool in {"session_send", "session_steer"}:
                self._watch_control_operation(host_id, result)

    def _update_tool_remote_session(
        self, tool: str, arguments: Mapping[str, Any], result: Mapping[str, Any]
    ) -> None:
        if tool != "session_wait":
            return
        operation_id = _optional_identifier(arguments.get("operationId"))
        if operation_id is None:
            return
        identity = self._operation_sessions.get(operation_id)
        host_id = _optional_identifier(arguments.get("hostId"))
        if identity is not None:
            if host_id is None:
                host_id = identity[0]
            elif host_id != identity[0]:
                return
        host_name = _optional_hostname(result.get("hostName"))
        if host_name is None and identity is not None:
            host_name = identity[2]
        thread_id = _optional_identifier(result.get("threadId"))
        if thread_id is None and identity is not None:
            thread_id = identity[1]
        nested_result = result.get("result")
        if thread_id is None and isinstance(nested_result, Mapping):
            thread_id = _optional_identifier(nested_result.get("threadId"))
        if host_id is None or thread_id is None:
            return
        if self._active_operations.get((host_id, thread_id)) not in {
            None,
            operation_id,
        }:
            return
        remote_session_id = self._register_remote_session(host_id, thread_id, host_name)
        if remote_session_id is None:
            return
        self._operation_sessions[operation_id] = (host_id, thread_id, host_name)
        turn_id = _optional_identifier(result.get("turnId"))
        if turn_id is None and isinstance(nested_result, Mapping):
            turn_id = _optional_identifier(nested_result.get("turnId"))
        events = result.get("events")
        if isinstance(events, list):
            for event in events:
                if isinstance(event, Mapping):
                    self._publish_remote_session_update(
                        remote_session_id,
                        event,
                        turn_id,
                    )
        if result.get("stopReason") == "terminal":
            self._publish_remote_session_update(
                remote_session_id,
                {"status": result.get("state", "completed")},
                turn_id,
            )

    def _remember_host_metadata(self, tool: str, result: Mapping[str, Any]) -> None:
        if tool != "hosts_list":
            return
        data = result.get("data")
        if not isinstance(data, list):
            return
        for host in data:
            if not isinstance(host, Mapping):
                continue
            host_id = _optional_identifier(host.get("hostId"))
            hostname = _optional_hostname(host.get("hostname"))
            if host_id is not None and hostname is not None:
                self._remote_hostnames[host_id] = hostname

    def _register_remote_session(
        self, host_id: str, thread_id: str, host_name: str | None = None
    ) -> str | None:
        if self._app_server_request is None or self._script_registration_id is None:
            return None
        identity = (host_id, thread_id)
        existing = self._remote_sessions.get(identity)
        if existing is not None:
            return existing
        if host_name is None:
            host_name = self._remote_hostnames.get(host_id)
        response = self._app_server_request(
            _REMOTE_SESSION_REGISTER_METHOD,
            {
                "registrationId": self._script_registration_id,
                "hostId": host_id,
                "remoteThreadId": thread_id,
                "hostName": host_name,
            },
        )
        remote_session = response.get("remoteSession")
        if isinstance(remote_session, Mapping):
            remote_session_id = _bounded_identifier(
                remote_session.get("remoteSessionId")
            )
        else:
            remote_session_id = _bounded_identifier(response.get("remoteSessionId"))
        self._remote_sessions[identity] = remote_session_id
        return remote_session_id

    def _publish_remote_session_update(
        self,
        remote_session_id: str,
        event: Mapping[str, Any],
        turn_id: str | None,
    ) -> None:
        if self._app_server_request is None or self._script_registration_id is None:
            return
        status = event.get("status")
        if status not in {"running", "completed", "failed", "cancelled", "expired"}:
            return
        params: dict[str, Any] = {
            "registrationId": self._script_registration_id,
            "remoteSessionId": remote_session_id,
            "status": status,
        }
        summary = event.get("activitySummary")
        if isinstance(summary, str) and summary:
            params["activitySummary"] = summary[:64]
        output_delta = event.get("outputDelta")
        output_cursor = event.get("outputCursor")
        if isinstance(output_delta, str) and isinstance(output_cursor, str):
            params["outputDelta"] = output_delta[:_REMOTE_SESSION_UPDATE_BYTES]
            params["outputCursor"] = _bounded_identifier(output_cursor)
        items = event.get("items")
        if isinstance(items, list) and items:
            params["items"] = items
            forwarded = self._forwarded_items.setdefault(remote_session_id, set())
            for entry in items:
                if len(forwarded) < _MAX_FORWARDED_ITEMS and isinstance(entry, Mapping):
                    forwarded.add(entry_key(entry))
        if turn_id is not None:
            params["remoteTurnId"] = turn_id
            self._remote_session_turns[remote_session_id] = turn_id
        self._pending_remote_session_updates.put(params)

    def flush_remote_session_updates(self) -> None:
        """Send watcher updates through the child IPC owner thread."""

        if self._app_server_request is None:
            return
        while True:
            try:
                params = self._pending_remote_session_updates.get_nowait()
            except queue.Empty:
                return
            self._app_server_request(_REMOTE_SESSION_UPDATE_METHOD, params)


def _tool_name(namespace: Any, tool: Any) -> str:
    tool = _bounded_identifier(tool)
    if namespace is None:
        if not tool.startswith("remote_"):
            raise BrokerError.not_found()
        return tool.removeprefix("remote_")
    if namespace == _REMOTE_NAMESPACE and not tool.startswith("remote_"):
        return tool
    raise BrokerError.not_found()


def _optional_identifier(value: Any) -> str | None:
    try:
        return _bounded_identifier(value)
    except BrokerError:
        return None


def _require_completed_control_operation(
    action: str, result: Mapping[str, Any]
) -> None:
    state = result.get("state")
    expected_state = "cancelled" if action == "cancel" else "completed"
    if state != expected_state:
        raise BrokerError.internal()


def _optional_hostname(value: Any) -> str | None:
    if (
        isinstance(value, str)
        and value
        and len(value.encode("utf-8")) <= 255
        and not any(character.isspace() for character in value)
    ):
        return value
    return None


def _broker_timeout(tool: str, arguments: Mapping[str, Any]) -> float | None:
    if tool == "session_start":
        return _SESSION_START_TIMEOUT_SECONDS
    if tool not in {"hosts_discover", "session_wait"}:
        return None
    timeout = arguments.get("timeoutSeconds")
    if (
        not isinstance(timeout, (int, float))
        or isinstance(timeout, bool)
        or timeout < 0
        or timeout > _MAX_WAIT_SECONDS
    ):
        return None
    return max(1.0, float(timeout) + _WAIT_TIMEOUT_BUFFER_SECONDS)


def _model_visible_result(tool: str, result: Mapping[str, Any]) -> dict[str, Any]:
    if tool == "sessions_list":
        return {
            "data": [_without_cwd(session) for session in _result_data(result)],
            "nextCursor": result.get("nextCursor"),
        }
    if tool == "sessions_search":
        return {
            "data": [
                {
                    "thread": _without_cwd(entry.get("thread")),
                    "snippet": entry.get("snippet", ""),
                }
                for entry in _result_data(result)
                if isinstance(entry, Mapping)
            ],
            "nextCursor": result.get("nextCursor"),
        }
    return without_items(dict(result))


def _result_data(result: Mapping[str, Any]) -> list[Mapping[str, Any]]:
    data = result.get("data")
    if not isinstance(data, list):
        raise BrokerError.internal()
    return [entry for entry in data if isinstance(entry, Mapping)]


def _without_cwd(value: Any) -> dict[str, Any]:
    if not isinstance(value, Mapping):
        raise BrokerError.internal()
    return {key: item for key, item in value.items() if key != "cwd"}


def main(_argv: Sequence[str] | None = None) -> int:
    sdk = _load_session_script_sdk()
    thread_id = os.environ.get("XEDOC_SESSION_SCRIPT_THREAD_ID")
    extension_id = os.environ.get("XEDOC_SESSION_SCRIPT_ID")
    binding_token = os.environ.get("XEDOC_REMOTE_AGENT_BINDING_TOKEN")
    if thread_id is None or extension_id is None or binding_token is None:
        print("remote-agent extension host identity is unavailable", file=sys.stderr)
        return 1

    client = sdk.SessionScriptClient.from_host_child()
    controller_id = controller_identifier(load_bootstrap_descriptor())
    broker = LocalIpcClient(
        max_message_bytes=_MAX_ARGUMENT_BYTES,
        max_result_bytes=_MAX_RESULT_BYTES,
        controller_id=controller_id,
    )
    extension = RemoteAgentExtension(
        broker,
        thread_id=thread_id,
        extension_id=extension_id,
        app_server_request=lambda method, params: client.request(
            method, dict(params)
        ),
    )
    client.set_server_request_handler(extension.handle_request)
    source_lease: str | None = None
    try:
        client.initialize(
            client_name="xedoc-remote-agent-extension",
            title="Xedoc remote agent",
            version=_VERSION,
        )
        handshake = broker.handshake()
        if handshake != {
            "hostId": handshake.get("hostId"),
            "protocol": PROTOCOL,
            "version": PROTOCOL_VERSION,
            "status": "available",
            "controllerId": controller_id,
        }:
            raise BrokerError.unavailable()
        _bounded_identifier(handshake["hostId"])
        registered = client.register(
            thread_id=thread_id,
            script_id=extension_id,
            name="Xedoc remote agent",
            version=_VERSION,
            subscriptions={
                "modelResponseDeltas": False,
                "modelResponseCompleted": False,
                "userMessages": False,
                "turnCompleted": False,
                "prompts": [],
                "sessionUpdates": False,
            },
            requested_capabilities=[],
        )
        registration_id = registered.get("registrationId")
        if not isinstance(registration_id, str):
            raise RuntimeError("script registration did not return an identifier")
        extension.set_script_registration(registration_id)
        source_lease = broker.bind_extension(thread_id, extension_id, binding_token)
        extension.set_source_lease(source_lease)
        client.read(registration_id)
        while True:
            extension.flush_remote_session_updates()
            if client.has_buffered_message():
                client.handle_message(client.receive_message())
                continue
            readable, _, _ = select.select([sys.stdin], [], [], 0.1)
            if readable:
                client.handle_message(client.receive_message())
    except (BrokerError, sdk.RpcError, EOFError, OSError, RuntimeError) as error:
        message = str(error).encode("utf-8", "replace")[:512].decode(
            "utf-8", "ignore"
        )
        print(f"remote-agent extension stopped: {message}", file=sys.stderr)
        return 1
    finally:
        if source_lease is not None:
            try:
                broker.unbind_extension(source_lease)
            except BrokerError:
                pass
        client.close()


if __name__ == "__main__":
    raise SystemExit(main())
