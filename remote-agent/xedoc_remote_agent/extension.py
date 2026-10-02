"""Thread-scoped built-in remote-agent extension child."""

from __future__ import annotations

import json
import os
import sys
from typing import Any, Mapping, Sequence

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
_REMOTE_NAMESPACE = "remote"
_LOCAL_METHODS = {
    "hosts_list": "host/list",
    "hosts_discover": "host/discover",
    "host_pair": "host/pair",
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
    "session_send": "session/send",
    "session_message": "session/message",
    "session_status": "session/status",
    "session_wait": "session/wait",
    "session_cancel": "session/cancel",
    "session_detach": "session/detach",
    "request_review": "request/review",
    "request_approve": "request/approve",
    "request_reject": "request/reject",
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


class RemoteAgentExtension:
    """Validate dynamic tool calls and dispatch only fixed local broker methods."""

    def __init__(
        self,
        broker: LocalIpcClient,
        *,
        thread_id: str,
        extension_id: str,
        source_lease: str | None = None,
    ) -> None:
        self._broker = broker
        self._thread_id = _bounded_identifier(thread_id)
        self._extension_id = _bounded_identifier(extension_id)
        self._source_lease = (
            _bounded_identifier(source_lease) if source_lease is not None else None
        )

    def set_source_lease(self, source_lease: str) -> None:
        """Attach the broker-issued lease after host registration succeeds."""

        self._source_lease = _bounded_identifier(source_lease)

    def handle_request(self, request: dict[str, Any]) -> dict[str, Any]:
        try:
            return self._handle_request(request)
        except BrokerError as error:
            return _failed(error)
        except BaseException:
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
            return _response(
                {"status": "ok", "result": _model_visible_result(tool, result)}, True
            )
        raise BrokerError.not_found()


def _tool_name(namespace: Any, tool: Any) -> str:
    tool = _bounded_identifier(tool)
    if namespace is None:
        if not tool.startswith("remote_"):
            raise BrokerError.not_found()
        return tool.removeprefix("remote_")
    if namespace == _REMOTE_NAMESPACE and not tool.startswith("remote_"):
        return tool
    raise BrokerError.not_found()


def _broker_timeout(tool: str, arguments: Mapping[str, Any]) -> float | None:
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
        broker, thread_id=thread_id, extension_id=extension_id
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
