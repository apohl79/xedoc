"""Thread-scoped built-in remote-agent extension child."""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Callable, Mapping, Sequence

from .errors import BrokerError
from .ipc import PROTOCOL
from .ipc import PROTOCOL_VERSION
from .ipc import LocalIpcClient
from .workspaces import controller_identifier
from .workspaces import load_bootstrap_descriptor


_VERSION = "0.1.0"
_MAX_ARGUMENT_BYTES = 64 * 1024
_MAX_RESULT_BYTES = 256 * 1024
_MAX_IDENTIFIER_LENGTH = 128
_MAX_WAIT_SECONDS = 3_600
_WAIT_TIMEOUT_BUFFER_SECONDS = 5.0
_SESSION_START_TIMEOUT_SECONDS = 30.0
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
        self._operation_sessions: dict[str, tuple[str, str | None]] = {}

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
        required = _REMOTE_SESSION_CONTROL_BASE_FIELDS | allowed_values
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
            broker_params["turnId"] = _bounded_identifier(params["expectedTurnId"])

        method = f"session/{action}"
        result = self._broker.call(
            method,
            broker_params,
            timeout_seconds=(
                _SESSION_START_TIMEOUT_SECONDS if action == "attach" else None
            ),
        )
        if action == "attach":
            result = self._await_control_operation(host_id, result)
        if action in {"send", "steer"}:
            self._register_remote_session(host_id, thread_id)
        return {"success": True, "result": result}

    def _await_control_operation(
        self, host_id: str, result: Mapping[str, Any]
    ) -> dict[str, Any]:
        operation_id = _bounded_identifier(result.get("operationId"))
        return self._broker.call(
            "session/wait",
            {
                "hostId": host_id,
                "operationId": operation_id,
                "timeoutSeconds": _SESSION_START_TIMEOUT_SECONDS,
            },
            timeout_seconds=_SESSION_START_TIMEOUT_SECONDS + _WAIT_TIMEOUT_BUFFER_SECONDS,
        )

    def _register_tool_remote_session(
        self, tool: str, arguments: Mapping[str, Any], result: Mapping[str, Any]
    ) -> None:
        host_id = _optional_identifier(arguments.get("hostId"))
        if host_id is None:
            return
        thread_id = _optional_identifier(arguments.get("threadId"))
        operation_id = _optional_identifier(result.get("operationId"))
        if tool in {"session_resume", "session_attach", "session_send", "session_steer"}:
            if thread_id is None:
                return
            self._register_remote_session(host_id, thread_id)
        if operation_id is not None:
            self._operation_sessions[operation_id] = (host_id, thread_id)

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
        thread_id = _optional_identifier(result.get("threadId"))
        if thread_id is None and identity is not None:
            thread_id = identity[1]
        nested_result = result.get("result")
        if thread_id is None and isinstance(nested_result, Mapping):
            thread_id = _optional_identifier(nested_result.get("threadId"))
        if host_id is None or thread_id is None:
            return
        remote_session_id = self._register_remote_session(host_id, thread_id)
        if remote_session_id is None:
            return
        self._operation_sessions[operation_id] = (host_id, thread_id)
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

    def _register_remote_session(self, host_id: str, thread_id: str) -> str | None:
        if self._app_server_request is None or self._script_registration_id is None:
            return None
        identity = (host_id, thread_id)
        existing = self._remote_sessions.get(identity)
        if existing is not None:
            return existing
        response = self._app_server_request(
            _REMOTE_SESSION_REGISTER_METHOD,
            {
                "registrationId": self._script_registration_id,
                "hostId": host_id,
                "remoteThreadId": thread_id,
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
            params["outputDelta"] = output_delta[:4_096]
            params["outputCursor"] = _bounded_identifier(output_cursor)
        if turn_id is not None:
            params["remoteTurnId"] = turn_id
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
    return dict(result)


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
