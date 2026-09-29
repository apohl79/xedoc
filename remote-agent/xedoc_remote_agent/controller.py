"""Normal local app-server controller connection used by the broker."""

from __future__ import annotations

from pathlib import Path
import threading
from typing import Any, Callable, Mapping

from .catalog import SessionCatalog
from .errors import BrokerError, map_controller_error
from .models import BrokerConfig
from .workspaces import (
    WorkspaceRegistry,
    load_bootstrap_descriptor,
    load_broker_config,
)


class _ControllerConnection:
    """Thin, bounded adapter over ``SessionScriptClient``'s controller scope."""

    CLIENT_NAME = "xedoc-remote-agentd"
    CLIENT_TITLE = "Xedoc Remote Agent"
    CLIENT_VERSION = "0.1.0"

    def __init__(self, client: Any) -> None:
        self._client = client
        self._closed = False
        self._client_lock = threading.RLock()

    @classmethod
    def _connect_from_bootstrap(
        cls,
        *,
        xedoc_home: str | Path | None = None,
        timeout: float = 10.0,
        client_factory: Callable[[Path, float], Any] | None = None,
        version: str | None = None,
    ) -> tuple["_ControllerConnection", BrokerConfig]:
        """Open only the endpoint recorded in host-only bootstrap TOML."""

        if not isinstance(timeout, (int, float)) or isinstance(timeout, bool) or timeout <= 0:
            raise BrokerError.invalid_request()
        descriptor = load_bootstrap_descriptor(xedoc_home=xedoc_home)
        client: Any = None
        connection: _ControllerConnection | None = None
        try:
            if client_factory is None:
                client_factory = _default_client_factory
            client = client_factory(descriptor.socket_path, float(timeout))
            connection = cls(client)
            with connection._client_lock:
                client.initialize(
                    cls.CLIENT_NAME,
                    cls.CLIENT_TITLE,
                    version or cls.CLIENT_VERSION,
                )
                # Do not pass cwd or ask for project layers. The Rust
                # app-server resolves this as a host-wide config/read.
                response = client.request("config/read", {})
            config = load_broker_config(response)
            return connection, config
        except BrokerError:
            if connection is not None:
                connection._close()
            else:
                _close_quietly(client)
            raise
        except BaseException as error:
            if connection is not None:
                connection._close()
            else:
                _close_quietly(client)
            raise map_controller_error(error) from error

    def _request(self, method: str, params: Mapping[str, Any]) -> dict[str, Any]:
        with self._client_lock:
            if self._closed or not isinstance(method, str) or not method:
                raise BrokerError.unavailable()
            if not isinstance(params, Mapping):
                raise BrokerError.invalid_request()
            try:
                response = self._client.request(method, dict(params))
            except BaseException as error:
                raise map_controller_error(error) from error
        if not isinstance(response, dict):
            raise BrokerError.internal()
        return response

    def _list_sessions(
        self, *, cursor: str | None = None, limit: int | None = None
    ) -> dict[str, Any]:
        params: dict[str, Any] = {"sortKey": "updated_at"}
        if cursor is not None:
            params["cursor"] = cursor
        if limit is not None:
            params["limit"] = limit
        return self._request("thread/list", params)

    def _search_sessions(
        self,
        search_term: str,
        *,
        cursor: str | None = None,
        limit: int | None = None,
    ) -> dict[str, Any]:
        params: dict[str, Any] = {"searchTerm": search_term, "sortKey": "updated_at"}
        if cursor is not None:
            params["cursor"] = cursor
        if limit is not None:
            params["limit"] = limit
        return self._request("thread/search", params)

    def _start_session(self, cwd: str) -> dict[str, Any]:
        return self._request("thread/start", {"cwd": cwd})

    def _resume_session(self, thread_id: str) -> dict[str, Any]:
        return self._request("thread/resume", {"threadId": thread_id})

    def _attach_session(self, thread_id: str) -> dict[str, Any]:
        return self._request("thread/resume", {"threadId": thread_id})

    def _read_session(self, thread_id: str, *, include_turns: bool = False) -> dict[str, Any]:
        return self._request(
            "thread/read",
            {"threadId": thread_id, "includeTurns": include_turns},
        )

    def _start_turn(self, thread_id: str, message: str) -> dict[str, Any]:
        return self._request(
            "turn/start",
            {"threadId": thread_id, "input": [{"type": "text", "text": message}]},
        )

    def _steer_turn(
        self,
        thread_id: str,
        expected_turn_id: str,
        message: str,
        client_message_id: str,
    ) -> dict[str, Any]:
        return self._request(
            "turn/steer",
            {
                "threadId": thread_id,
                "expectedTurnId": expected_turn_id,
                "clientUserMessageId": client_message_id,
                "input": [{"type": "text", "text": message}],
            },
        )

    def _interrupt_turn(self, thread_id: str, turn_id: str) -> dict[str, Any]:
        return self._request(
            "turn/interrupt", {"threadId": thread_id, "turnId": turn_id}
        )

    def _unsubscribe(self, thread_id: str) -> dict[str, Any]:
        return self._request("thread/unsubscribe", {"threadId": thread_id})

    def _close(self) -> None:
        with self._client_lock:
            if self._closed:
                return
            self._closed = True
            _close_quietly(self._client)


class BrokerController:
    """Configured controller plus the validated host workspace policy."""

    def __init__(
        self,
        connection: _ControllerConnection,
        config: BrokerConfig,
        *,
        host_id: str = "host_local",
    ) -> None:
        if not isinstance(connection, _ControllerConnection) or not isinstance(
            config, BrokerConfig
        ):
            raise BrokerError.invalid_request()
        if not isinstance(host_id, str) or not host_id or len(host_id) > 128:
            raise BrokerError.invalid_request()
        self._connection = connection
        self.config = config
        self.workspaces = WorkspaceRegistry(config.workspaces)
        self.limits = config.limits
        self.host_id = host_id
        self._catalog = SessionCatalog(
            connection,
            self.workspaces,
            host_id=host_id,
            max_result_bytes=self.limits.max_result_bytes,
        )

    @classmethod
    def connect(
        cls,
        *,
        xedoc_home: str | Path | None = None,
        timeout: float = 10.0,
        client_factory: Callable[[Path, float], Any] | None = None,
        version: str | None = None,
        host_id: str = "host_local",
    ) -> "BrokerController":
        connection, config = _ControllerConnection._connect_from_bootstrap(
            xedoc_home=xedoc_home,
            timeout=timeout,
            client_factory=client_factory,
            version=version,
        )
        return cls(connection, config, host_id=host_id)

    def list_sessions(
        self, *, cursor: str | None = None, limit: int | None = None
    ) -> dict[str, Any]:
        return self._catalog.list_sessions(cursor=cursor, limit=limit).to_dict()

    def search_sessions(
        self,
        search_term: str,
        *,
        cursor: str | None = None,
        limit: int | None = None,
    ) -> dict[str, Any]:
        return self._catalog.search_sessions(
            search_term, cursor=cursor, limit=limit
        ).to_dict()

    def close(self) -> None:
        self._connection._close()


def _default_client_factory(socket_path: Path, timeout: float) -> Any:
    try:
        from scripts.session_script_sdk import SessionScriptClient
    except ImportError as error:
        raise BrokerError.unavailable() from error
    return SessionScriptClient.connect_unix_socket(socket_path, timeout)


def _close_quietly(client: Any) -> None:
    if client is None:
        return
    try:
        client.close()
    except BaseException:
        return
