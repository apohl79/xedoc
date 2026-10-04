"""Capability-authenticated private local IPC for the host broker.

Frames are a four-byte big-endian length followed by UTF-8 JSON. Request
frames are capped at ``max_message_bytes + 16 KiB`` and response frames at
``max_result_bytes + 16 KiB``. Both caps also have a hard 4 MiB ceiling.
"""

from __future__ import annotations

from dataclasses import asdict, is_dataclass
import hashlib
import hmac
import json
import os
from pathlib import Path
import secrets
import socket
import stat
import struct
import threading
import time
from typing import Any, Callable, Mapping

from .errors import BrokerError, ErrorCode
from .extension_bindings import ExtensionBindingRegistry
from .messages import MessageService
from .models import MAX_ID_LENGTH


PROTOCOL = "xedoc.remote-agent.local"
PROTOCOL_VERSION = 1
SOCKET_NAME = "broker.sock"
CAPABILITY_NAME = "broker.capability"
BINDING_CAPABILITY_NAME = "extension.capability"
MAX_CLIENTS = 16
CLIENT_IO_TIMEOUT_SECONDS = 1.0
MAX_CLIENT_CALL_TIMEOUT_SECONDS = 3_605.0
_FRAME_OVERHEAD = 16 * 1024
_HARD_FRAME_LIMIT = 4 * 1024 * 1024
_CAPABILITY_BYTES = 48
_REQUEST_FIELDS = {
    "capability",
    "requestId",
    "method",
    "params",
    "extensionLease",
    "controllerId",
}
_REQUIRED_REQUEST_FIELDS = {"capability", "requestId", "method", "params"}
_LIST_FIELDS = {"cursor", "limit"}
_SEARCH_FIELDS = {"query", "cursor", "limit"}
_REMOTE_LIST_FIELDS = _LIST_FIELDS | {"hostId"}
_REMOTE_SEARCH_FIELDS = _SEARCH_FIELDS | {"hostId"}
_SESSION_OPERATION_FIELDS: dict[str, tuple[set[str], set[str]]] = {
    "session/start": ({"hostId", "workspaceId", "relativePath"}, {"workspaceId"}),
    "session/resume": ({"hostId", "threadId"}, {"threadId"}),
    "session/attach": ({"hostId", "threadId"}, {"threadId"}),
    "session/send": ({"hostId", "threadId", "message"}, {"threadId", "message"}),
    "session/status": ({"hostId", "threadId"}, {"threadId"}),
    "session/wait": (
        {"hostId", "operationId", "timeoutSeconds"},
        {"operationId", "timeoutSeconds"},
    ),
    "session/cancel": (
        {"hostId", "threadId", "turnId"},
        {"threadId", "turnId"},
    ),
    "session/detach": ({"hostId", "threadId"}, {"threadId"}),
}


class LocalIpcServer:
    """Serve the explicit Stage 2 broker API over a private Unix socket."""

    def __init__(
        self,
        catalog: Any,
        operations: Any,
        *,
        host_id: str,
        max_message_bytes: int,
        max_result_bytes: int,
        peer_service: Any | None = None,
        message_service: MessageService | None = None,
        shutdown_callback: Callable[[], None] | None = None,
        xedoc_home: str | os.PathLike[str] | None = None,
        controller_id: str | None = None,
        max_clients: int = MAX_CLIENTS,
    ) -> None:
        _require_unix_sockets()
        if (
            not isinstance(host_id, str)
            or not host_id
            or len(host_id) > MAX_ID_LENGTH
            or not isinstance(max_clients, int)
            or isinstance(max_clients, bool)
            or not 0 < max_clients <= 256
        ):
            raise BrokerError.invalid_request()
        self._catalog = catalog
        self._operations = operations
        self._peer_service = peer_service
        if message_service is not None and not isinstance(message_service, MessageService):
            raise BrokerError.invalid_request()
        if shutdown_callback is not None and not callable(shutdown_callback):
            raise BrokerError.invalid_request()
        self._message_service = message_service
        self._shutdown_callback = shutdown_callback
        self.controller_id = (
            _bounded_identifier(controller_id) if controller_id is not None else None
        )
        self.host_id = host_id
        self._bindings = ExtensionBindingRegistry(host_id)
        self.max_request_bytes = _frame_limit(max_message_bytes)
        self.max_response_bytes = _frame_limit(max_result_bytes)
        home = _xedoc_home(xedoc_home)
        self.directory = home / "remote-agent"
        self.socket_path = self.directory / SOCKET_NAME
        self.capability_path = self.directory / CAPABILITY_NAME
        self.binding_capability_path = self.directory / BINDING_CAPABILITY_NAME
        self._capability: str | None = None
        self._binding_capability: str | None = None
        self._listener: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stopping = threading.Event()
        self._clients = threading.BoundedSemaphore(max_clients)
        self._workers: set[threading.Thread] = set()
        self._connections: set[socket.socket] = set()
        self._workers_lock = threading.Lock()
        self._shutdown_incomplete = False
        self._shutdown_complete = threading.Event()
        self._shutdown_complete.set()
        self._reaper: threading.Thread | None = None

    @property
    def running(self) -> bool:
        return self._listener is not None

    @property
    def shutdown_complete(self) -> bool:
        """Whether every server and client worker has exited after ``stop``."""

        return self._shutdown_complete.is_set()

    def wait_shutdown(self, timeout: float | None = None) -> bool:
        """Wait for the explicit post-stop worker-completion signal."""

        return self._shutdown_complete.wait(timeout)

    def start(self) -> None:
        if self.running:
            raise BrokerError.conflict()
        if self._shutdown_incomplete:
            raise BrokerError.unavailable()
        _prepare_directory(self.directory)
        _prepare_endpoint(self.socket_path)
        capability = secrets.token_urlsafe(_CAPABILITY_BYTES)
        binding_capability = secrets.token_urlsafe(_CAPABILITY_BYTES)
        _write_capability(self.capability_path, capability)
        try:
            _write_capability(self.binding_capability_path, binding_capability)
        except BrokerError:
            _safe_unlink(self.capability_path, require_socket=False)
            raise
        listener: socket.socket | None = None
        try:
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(os.fspath(self.socket_path))
            _chmod_private(self.socket_path, 0o600)
            listener.listen(MAX_CLIENTS)
            listener.settimeout(0.2)
        except (AttributeError, NotImplementedError, OSError) as error:
            if listener is not None:
                listener.close()
            _safe_unlink(self.socket_path, require_socket=True)
            _safe_unlink(self.capability_path, require_socket=False)
            _safe_unlink(self.binding_capability_path, require_socket=False)
            raise BrokerError.unavailable() from error
        self._capability = capability
        self._binding_capability = binding_capability
        self._listener = listener
        self._stopping.clear()
        self._shutdown_complete.clear()
        self._thread = threading.Thread(
            target=self._serve, name="xedoc-remote-agent-ipc", daemon=True
        )
        self._thread.start()

    def stop(self) -> bool:
        """Stop accepting work and report whether all workers have exited."""

        self._stopping.set()
        listener, self._listener = self._listener, None
        if listener is not None:
            listener.close()
        deadline = time.monotonic() + 2.0
        server_thread = self._thread
        if server_thread is not None:
            server_thread.join(timeout=min(0.3, deadline - time.monotonic()))
            if not server_thread.is_alive():
                self._thread = None
        with self._workers_lock:
            workers = tuple(self._workers)
        cooperative_deadline = min(deadline, time.monotonic() + 0.2)
        for worker in workers:
            worker.join(timeout=max(0.0, cooperative_deadline - time.monotonic()))
        with self._workers_lock:
            connections = tuple(self._connections)
        for connection in connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            connection.close()
        with self._workers_lock:
            workers = tuple(self._workers)
        for worker in workers:
            worker.join(timeout=max(0.0, deadline - time.monotonic()))
        _safe_unlink(self.socket_path, require_socket=True)
        _safe_unlink(self.capability_path, require_socket=False)
        _safe_unlink(self.binding_capability_path, require_socket=False)
        self._capability = None
        self._binding_capability = None
        self._bindings.clear()
        self._update_shutdown_state()
        return self.shutdown_complete

    def __enter__(self) -> "LocalIpcServer":
        self.start()
        return self

    def __exit__(self, *_: object) -> None:
        self.stop()

    def _serve(self) -> None:
        while not self._stopping.is_set():
            listener = self._listener
            if listener is None:
                return
            try:
                connection, _ = listener.accept()
            except TimeoutError:
                continue
            except OSError:
                if self._stopping.is_set():
                    return
                continue
            if not self._clients.acquire(blocking=False):
                connection.close()
                continue
            connection.settimeout(CLIENT_IO_TIMEOUT_SECONDS)
            worker = threading.Thread(
                target=self._handle_client,
                args=(connection,),
                name="xedoc-remote-agent-ipc-client",
                daemon=True,
            )
            with self._workers_lock:
                if self._stopping.is_set():
                    self._clients.release()
                    connection.close()
                    return
                self._workers.add(worker)
                self._connections.add(connection)
            worker.start()

    def _handle_client(self, connection: socket.socket) -> None:
        try:
            with connection:
                request_id = ""
                try:
                    request = _receive_json(connection, self.max_request_bytes)
                    request_id = _request_id(request)
                    response = self._dispatch(request, request_id)
                except TimeoutError:
                    response = _error_response(
                        self.host_id, request_id, BrokerError.unavailable()
                    )
                except BrokerError as error:
                    response = _error_response(self.host_id, request_id, error)
                except BaseException:
                    response = _error_response(
                        self.host_id, request_id, BrokerError.internal()
                    )
                try:
                    _send_json(connection, response, self.max_response_bytes)
                except (BrokerError, OSError):
                    return
        finally:
            self._clients.release()
            with self._workers_lock:
                self._workers.discard(threading.current_thread())
                self._connections.discard(connection)
            self._update_shutdown_state()

    def _update_shutdown_state(self) -> None:
        if not self._stopping.is_set():
            return
        with self._workers_lock:
            server_alive = self._thread is not None and self._thread.is_alive()
            workers_alive = any(worker.is_alive() for worker in self._workers)
            self._shutdown_incomplete = server_alive or workers_alive
            complete = not self._shutdown_incomplete
            if complete:
                self._thread = None
                self._shutdown_complete.set()
                return
            reaper = self._reaper
            if reaper is None or not reaper.is_alive():
                self._reaper = threading.Thread(
                    target=self._reap_shutdown,
                    name="xedoc-remote-agent-ipc-reaper",
                    daemon=True,
                )
                self._reaper.start()

    def _reap_shutdown(self) -> None:
        while self._stopping.is_set():
            server_thread = self._thread
            if server_thread is not None and server_thread is not threading.current_thread():
                server_thread.join(timeout=0.2)
            with self._workers_lock:
                workers = tuple(self._workers)
            for worker in workers:
                worker.join(timeout=0.2)
            with self._workers_lock:
                server_alive = self._thread is not None and self._thread.is_alive()
                workers_alive = any(worker.is_alive() for worker in self._workers)
                if server_alive or workers_alive:
                    continue
                self._thread = None
                self._shutdown_incomplete = False
                self._shutdown_complete.set()
                self._reaper = None
                return

    def _dispatch(
        self, request: Mapping[str, Any], request_id: str
    ) -> dict[str, Any]:
        if not _REQUIRED_REQUEST_FIELDS <= set(request) <= _REQUEST_FIELDS:
            raise BrokerError.invalid_request()
        capability = request["capability"]
        if (
            not isinstance(capability, str)
            or self._capability is None
            or not hmac.compare_digest(capability, self._capability)
        ):
            raise BrokerError.unauthorized()
        method = request["method"]
        params = request["params"]
        if not isinstance(method, str) or not isinstance(params, Mapping):
            raise BrokerError.invalid_request()
        extension_lease = request.get("extensionLease")
        requested_controller_id = request.get("controllerId")
        if requested_controller_id is not None:
            requested_controller_id = _bounded_identifier(requested_controller_id)
            if self.controller_id is None or not hmac.compare_digest(
                requested_controller_id, self.controller_id
            ):
                raise BrokerError.conflict()

        if method == "extension/bind":
            _require_absent_lease(extension_lease)
            _exact_fields(
                params,
                {"threadId", "extensionId", "bindingToken"},
                {"threadId", "extensionId", "bindingToken"},
            )
            thread_id = _bounded_identifier(params["threadId"])
            extension_id = _bounded_identifier(params["extensionId"])
            binding_token = params["bindingToken"]
            if (
                not isinstance(binding_token, str)
                or self._binding_capability is None
                or not hmac.compare_digest(
                    binding_token,
                    _extension_binding_token(
                        self._binding_capability, thread_id, extension_id
                    ),
                )
            ):
                raise BrokerError.unauthorized()
            result = {
                "lease": self._bindings.bind(
                    {"threadId": thread_id, "extensionId": extension_id}
                )
            }
        elif method == "extension/unbind":
            _exact_fields(params, set())
            self._bindings.revoke(extension_lease)
            result = {"status": "revoked"}
        elif method == "host/handshake":
            _require_absent_lease(extension_lease)
            _exact_fields(params, set())
            result: Any = {
                "hostId": self.host_id,
                "protocol": PROTOCOL,
                "version": PROTOCOL_VERSION,
                "status": "available",
            }
            if self.controller_id is not None:
                result["controllerId"] = self.controller_id
        elif method == "daemon/shutdown":
            _require_absent_lease(extension_lease)
            _exact_fields(params, set())
            callback = self._shutdown_callback
            if callback is None:
                raise BrokerError.unavailable()
            callback()
            result = {"status": "shutdownRequested"}
        elif method == "host/list":
            _require_absent_lease(extension_lease)
            _exact_fields(params, set())
            result = self._peer().hosts_list()
        elif method == "host/discover":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"timeoutSeconds", "endpoints"})
            result = self._peer().discover(params)
        elif method == "host/pair":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId", "role", "fingerprint"}, {"hostId", "role"})
            result = self._peer().pair(params)
        elif method == "enrollment/create":
            _require_absent_lease(extension_lease)
            _exact_fields(params, set())
            result = self._peer().enrollment_create(params)
        elif method == "enrollment/remember":
            _require_absent_lease(extension_lease)
            _exact_fields(
                params,
                {"hostId", "fingerprint", "code"},
                {"hostId", "fingerprint", "code"},
            )
            result = self._peer().enrollment_remember(params)
        elif method == "pairing/list":
            _require_absent_lease(extension_lease)
            _exact_fields(params, set())
            result = self._peer().pairing_requests(params)
        elif method in {"pairing/approve", "pairing/reject"}:
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId"}, {"hostId"})
            handler = getattr(
                self._peer(),
                "pairing_approve"
                if method == "pairing/approve"
                else "pairing_reject",
            )
            result = handler(params)
        elif method == "host/grants/list":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId"}, {"hostId"})
            result = self._peer().grants_list(params)
        elif method == "host/grants/set":
            _require_absent_lease(extension_lease)
            _exact_fields(
                params,
                {"hostId", "scopes", "workspaceId", "threadId", "expiresAt"},
                {"hostId", "scopes"},
            )
            result = self._peer().grant_set(params)
        elif method == "host/suspend":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId"}, {"hostId"})
            result = self._peer().suspend(params)
        elif method == "host/revoke":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId"}, {"hostId"})
            result = self._peer().revoke(params)
        elif method == "host/remove":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId"}, {"hostId"})
            result = self._peer().remove(params)
        elif method == "host/rotate":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId"}, {"hostId"})
            result = self._peer().rotate(params)
        elif method == "request/review":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"requestId"}, {"requestId"})
            result = self._peer().request_review(params)
        elif method in {"request/approve", "request/reject"}:
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"requestId", "reason"}, {"requestId"})
            handler = getattr(
                self._peer(),
                "request_approve" if method == "request/approve" else "request_reject",
            )
            result = handler(params)
        elif method == "workspace/list":
            _require_absent_lease(extension_lease)
            _exact_fields(params, {"hostId"})
            result = self._remote_or_local(
                method,
                params,
                lambda: {"data": list(self._catalog.workspace_list())},
            )
        elif method == "session/list":
            _require_absent_lease(extension_lease)
            _exact_fields(params, _REMOTE_LIST_FIELDS)
            result = self._remote_or_local(
                method,
                params,
                lambda: self._catalog.list_sessions(
                    cursor=params.get("cursor"),
                    limit=params.get("limit"),
                ).to_dict(),
            )
        elif method == "session/search":
            _require_absent_lease(extension_lease)
            _exact_fields(params, _REMOTE_SEARCH_FIELDS, {"query"})
            result = self._remote_or_local(
                method,
                params,
                lambda: self._catalog.search_sessions(
                    params["query"],
                    cursor=params.get("cursor"),
                    limit=params.get("limit"),
                ).to_dict(),
            )
        elif method == "session/message":
            result = self._messages().submit_local(
                self._bindings.resolve(extension_lease), params
            )
        elif method in _SESSION_OPERATION_FIELDS:
            _require_absent_lease(extension_lease)
            allowed, required = _SESSION_OPERATION_FIELDS[method]
            _exact_fields(params, allowed, required)
            result = self._session_or_local(method, params)
        else:
            raise BrokerError.not_found()
        return {
            "hostId": self.host_id,
            "requestId": request_id,
            "status": "ok",
            "result": _json_value(result),
        }

    def _peer(self) -> Any:
        if self._peer_service is None:
            raise BrokerError.unavailable()
        return self._peer_service

    def _messages(self) -> MessageService:
        if self._message_service is None:
            raise BrokerError.unavailable()
        return self._message_service

    def _remote_or_local(
        self,
        method: str,
        params: Mapping[str, Any],
        local: Any,
    ) -> Any:
        host_id = params.get("hostId")
        if host_id is None or host_id == self.host_id:
            return local()
        if not isinstance(host_id, str):
            raise BrokerError.invalid_request()
        return self._peer().peer_read(method, params)

    def _session_or_local(self, method: str, params: Mapping[str, Any]) -> Any:
        host_id = params.get("hostId")
        operation = method.removeprefix("session/")
        peer_params = {key: value for key, value in params.items() if key != "hostId"}
        if host_id is None or host_id == self.host_id:
            handler = getattr(self._operations, operation, None)
            if not callable(handler):
                raise BrokerError.internal()
            interruptible_wait = getattr(
                self._operations, "_wait_interruptible", None
            )
            if method == "session/wait" and callable(interruptible_wait):
                return interruptible_wait(peer_params, self._stopping)
            return handler(peer_params)
        if not isinstance(host_id, str):
            raise BrokerError.invalid_request()
        return self._peer().peer_session(method, host_id, peer_params)


class LocalIpcClient:
    """One-request-per-connection client for the private broker endpoint."""

    def __init__(
        self,
        *,
        xedoc_home: str | os.PathLike[str] | None = None,
        max_message_bytes: int,
        max_result_bytes: int,
        timeout_seconds: float = 5.0,
        controller_id: str | None = None,
    ) -> None:
        _require_unix_sockets()
        if (
            not isinstance(timeout_seconds, (int, float))
            or isinstance(timeout_seconds, bool)
            or timeout_seconds <= 0
            or timeout_seconds > 120
        ):
            raise BrokerError.invalid_request()
        directory = _xedoc_home(xedoc_home) / "remote-agent"
        _reject_symlink_components(directory)
        _validate_private_directory(directory)
        self.socket_path = directory / SOCKET_NAME
        self.capability_path = directory / CAPABILITY_NAME
        self.max_request_bytes = _frame_limit(max_message_bytes)
        self.max_response_bytes = _frame_limit(max_result_bytes)
        self.timeout_seconds = float(timeout_seconds)
        self.controller_id = (
            _bounded_identifier(controller_id) if controller_id is not None else None
        )

    def handshake(self) -> dict[str, Any]:
        return self.call("host/handshake", {})

    def call(
        self,
        method: str,
        params: Mapping[str, Any],
        *,
        request_id: str | None = None,
        timeout_seconds: float | None = None,
        extension_lease: str | None = None,
    ) -> dict[str, Any]:
        if not isinstance(method, str) or not isinstance(params, Mapping):
            raise BrokerError.invalid_request()
        timeout = _client_call_timeout(timeout_seconds, self.timeout_seconds)
        capability = _read_capability(self.capability_path)
        request_id = request_id or f"req_{secrets.token_urlsafe(18)}"
        _bounded_identifier(request_id)
        _validate_client_endpoint(self.socket_path)
        request = {
            "capability": capability,
            "requestId": request_id,
            "method": method,
            "params": dict(params),
        }
        if extension_lease is not None:
            request["extensionLease"] = _bounded_identifier(extension_lease)
        if self.controller_id is not None:
            request["controllerId"] = self.controller_id
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.settimeout(timeout)
                connection.connect(os.fspath(self.socket_path))
                _send_json(connection, request, self.max_request_bytes)
                response = _receive_json(connection, self.max_response_bytes)
        except BrokerError:
            raise
        except (AttributeError, NotImplementedError, OSError) as error:
            raise BrokerError.unavailable() from error
        return _parse_response(response, request_id)

    def bind_extension(
        self, thread_id: str, extension_id: str, binding_token: str
    ) -> str:
        """Create one ephemeral broker provenance lease for this child."""

        result = self.call(
            "extension/bind",
            {
                "threadId": thread_id,
                "extensionId": extension_id,
                "bindingToken": _bounded_identifier(binding_token),
            },
        )
        lease = result.get("lease")
        return _bounded_identifier(lease)

    def unbind_extension(self, lease: str) -> None:
        """Revoke one child lease during normal shutdown."""

        self.call("extension/unbind", {}, extension_lease=lease)


def _request_id(request: Any) -> str:
    if not isinstance(request, Mapping):
        raise BrokerError.invalid_request()
    if set(request) - _REQUEST_FIELDS:
        raise BrokerError.invalid_request()
    return _bounded_identifier(request.get("requestId"))


def _bounded_identifier(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value) > MAX_ID_LENGTH:
        raise BrokerError.invalid_request()
    return value


def _extension_binding_token(
    binding_capability: str, thread_id: str, extension_id: str
) -> str:
    digest = hashlib.sha256()
    digest.update(binding_capability.encode("utf-8"))
    digest.update(b"\0")
    digest.update(thread_id.encode("utf-8"))
    digest.update(b"\0")
    digest.update(extension_id.encode("utf-8"))
    return digest.hexdigest()


def _client_call_timeout(value: float | None, default: float) -> float:
    timeout = default if value is None else value
    if (
        not isinstance(timeout, (int, float))
        or isinstance(timeout, bool)
        or timeout <= 0
        or timeout > MAX_CLIENT_CALL_TIMEOUT_SECONDS
    ):
        raise BrokerError.invalid_request()
    return float(timeout)


def _exact_fields(
    value: Mapping[str, Any], allowed: set[str], required: set[str] | None = None
) -> None:
    required = required or set()
    if set(value) - allowed or not required.issubset(value):
        raise BrokerError.invalid_request()


def _require_absent_lease(value: Any) -> None:
    if value is not None:
        raise BrokerError.invalid_request()


def _frame_limit(configured_bytes: int) -> int:
    if (
        not isinstance(configured_bytes, int)
        or isinstance(configured_bytes, bool)
        or configured_bytes <= 0
    ):
        raise BrokerError.invalid_request()
    return min(_HARD_FRAME_LIMIT, configured_bytes + _FRAME_OVERHEAD)


def _receive_json(connection: socket.socket, limit: int) -> Mapping[str, Any]:
    header = _receive_exact(connection, 4)
    length = struct.unpack(">I", header)[0]
    if length == 0 or length > limit:
        raise BrokerError.limit_exceeded()
    raw = _receive_exact(connection, length)
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as error:
        raise BrokerError.invalid_request() from error
    if not isinstance(value, Mapping):
        raise BrokerError.invalid_request()
    return value


def _receive_exact(connection: socket.socket, length: int) -> bytes:
    chunks: list[bytes] = []
    remaining = length
    while remaining:
        chunk = connection.recv(remaining)
        if not chunk:
            raise BrokerError.invalid_request()
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _send_json(connection: socket.socket, value: Any, limit: int) -> None:
    try:
        encoded = json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
    if not encoded or len(encoded) > limit:
        raise BrokerError.limit_exceeded()
    connection.sendall(struct.pack(">I", len(encoded)) + encoded)


def _json_value(value: Any) -> Any:
    if hasattr(value, "to_dict") and callable(value.to_dict):
        return value.to_dict()
    if is_dataclass(value):
        return asdict(value)
    if isinstance(value, tuple):
        return [_json_value(item) for item in value]
    return value


def _error_response(
    host_id: str, request_id: str, error: BrokerError
) -> dict[str, Any]:
    return {
        "hostId": host_id,
        "requestId": request_id,
        "status": "error",
        "error": error.to_dict(),
    }


def _parse_response(response: Mapping[str, Any], request_id: str) -> dict[str, Any]:
    required = {"hostId", "requestId", "status"}
    if not required.issubset(response) or response.get("requestId") != request_id:
        raise BrokerError.invalid_request()
    status = response.get("status")
    if status == "ok":
        if set(response) != required | {"result"}:
            raise BrokerError.invalid_request()
        result = response["result"]
        if not isinstance(result, Mapping):
            raise BrokerError.invalid_request()
        return dict(result)
    if status != "error" or set(response) != required | {"error"}:
        raise BrokerError.invalid_request()
    raw = response["error"]
    if not isinstance(raw, Mapping) or set(raw) != {"code", "message", "retryable"}:
        raise BrokerError.invalid_request()
    try:
        code = ErrorCode(raw["code"])
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    message = raw["message"]
    retryable = raw["retryable"]
    if not isinstance(message, str) or not isinstance(retryable, bool):
        raise BrokerError.invalid_request()
    raise BrokerError(code, message, retryable=retryable)


def _xedoc_home(value: str | os.PathLike[str] | None) -> Path:
    raw = value if value is not None else os.environ.get("XEDOC_HOME")
    home = Path(raw) if raw is not None else Path.home() / ".xedoc"
    if not home.is_absolute():
        raise BrokerError.invalid_request()
    _reject_symlink_components(home)
    return home


def _prepare_directory(directory: Path) -> None:
    _reject_symlink_components(directory.parent)
    try:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = directory.lstat()
    except OSError as error:
        raise BrokerError.unavailable() from error
    if not stat.S_ISDIR(info.st_mode) or _wrong_owner(info) or info.st_mode & 0o077:
        raise BrokerError.unauthorized()
    _chmod_private(directory, 0o700)


def _validate_private_directory(directory: Path) -> None:
    try:
        info = directory.lstat()
    except OSError as error:
        raise BrokerError.unavailable() from error
    if not stat.S_ISDIR(info.st_mode) or _wrong_owner(info) or info.st_mode & 0o077:
        raise BrokerError.unauthorized()


def _prepare_endpoint(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISSOCK(info.st_mode)
        or _wrong_owner(info)
        or info.st_mode & 0o077
    ):
        raise BrokerError.unauthorized()
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as probe:
            probe.settimeout(0.1)
            probe.connect(os.fspath(path))
    except OSError:
        _safe_unlink(path, require_socket=True)
        return
    raise BrokerError.conflict()


def _write_capability(path: Path, capability: str) -> None:
    _remove_safe_capability(path)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags, 0o600)
        with os.fdopen(descriptor, "w", encoding="ascii") as output:
            output.write(capability)
            output.write("\n")
        _chmod_private(path, 0o600)
    except OSError as error:
        raise BrokerError.unavailable() from error


def _remove_safe_capability(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
        or info.st_mode & 0o077
    ):
        raise BrokerError.unauthorized()
    try:
        path.unlink()
    except OSError as error:
        raise BrokerError.unavailable() from error


def _read_capability(path: Path) -> str:
    try:
        info = path.lstat()
        if (
            stat.S_ISLNK(info.st_mode)
            or not stat.S_ISREG(info.st_mode)
            or _wrong_owner(info)
            or info.st_mode & 0o077
            or info.st_size > 256
        ):
            raise BrokerError.unauthorized()
        capability = path.read_text(encoding="ascii").strip()
    except BrokerError:
        raise
    except (OSError, UnicodeError) as error:
        raise BrokerError.unavailable() from error
    if len(capability) < 32 or len(capability) > 128:
        raise BrokerError.unavailable()
    return capability


def _validate_client_endpoint(path: Path) -> None:
    try:
        info = path.lstat()
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISSOCK(info.st_mode)
        or _wrong_owner(info)
        or info.st_mode & 0o077
    ):
        raise BrokerError.unauthorized()


def _reject_symlink_components(path: Path) -> None:
    current = Path(path.anchor)
    for part in path.parts[1:]:
        current /= part
        try:
            if stat.S_ISLNK(current.lstat().st_mode):
                raise BrokerError.unauthorized()
        except FileNotFoundError:
            return
        except BrokerError:
            raise
        except OSError as error:
            raise BrokerError.unavailable() from error


def _safe_unlink(path: Path, *, require_socket: bool) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    except OSError:
        return
    expected = stat.S_ISSOCK(info.st_mode) if require_socket else stat.S_ISREG(info.st_mode)
    if expected and not stat.S_ISLNK(info.st_mode) and not _wrong_owner(info):
        try:
            path.unlink()
        except OSError:
            pass


def _wrong_owner(info: os.stat_result) -> bool:
    return hasattr(os, "getuid") and info.st_uid != os.getuid()


def _chmod_private(path: Path, mode: int) -> None:
    try:
        os.chmod(path, mode, follow_symlinks=False)
    except (NotImplementedError, OSError) as error:
        if os.name != "nt":
            raise BrokerError.unavailable() from error


def _require_unix_sockets() -> None:
    if not hasattr(socket, "AF_UNIX"):
        raise BrokerError.unavailable()
