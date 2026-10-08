"""Normal local app-server controller connection used by the broker."""

from __future__ import annotations

from collections import OrderedDict
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


_MAX_LIVE_TURNS = 16
_MAX_LIVE_ITEMS_PER_TURN = 256
_MAX_LIVE_STRING_CHARS = 64 * 1024
_MAX_LIVE_LIST_ITEMS = 64


def _bounded_live_value(value: Any, depth: int = 0) -> Any:
    """Copy an item with bounded strings and lists so live capture stays small."""

    if isinstance(value, str):
        if len(value) <= _MAX_LIVE_STRING_CHARS:
            return value
        return "…" + value[-(_MAX_LIVE_STRING_CHARS - 1) :]
    if depth >= 8:
        return None
    if isinstance(value, list):
        return [_bounded_live_value(v, depth + 1) for v in value[:_MAX_LIVE_LIST_ITEMS]]
    if isinstance(value, dict):
        return {
            str(key): _bounded_live_value(v, depth + 1)
            for key, v in list(value.items())[:_MAX_LIVE_LIST_ITEMS]
        }
    return value


class _LiveTurn:
    """Items the app-server announced for one turn this connection saw start."""

    def __init__(self) -> None:
        self.items: OrderedDict[str, dict[str, Any]] = OrderedDict()
        self.completed = False


class _LiveTurnItems:
    """Bounded record of live turn items.

    ``thread/read`` rebuilds turns from the rollout, which never stores command
    executions or other transient items. Only the live ``item/*`` notifications
    carry them, so the controller keeps what it observed for each turn.
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._turns: OrderedDict[tuple[str, str], _LiveTurn] = OrderedDict()

    def observe(self, message: Mapping[str, Any]) -> None:
        method = message.get("method")
        params = message.get("params")
        if not isinstance(params, Mapping) or not isinstance(
            params.get("threadId"), str
        ):
            return
        thread_id = params["threadId"]
        with self._lock:
            if method == "turn/started" or method == "turn/completed":
                turn = params.get("turn")
                turn_id = turn.get("id") if isinstance(turn, Mapping) else None
                if not isinstance(turn_id, str):
                    return
                if method == "turn/started":
                    self._turns[(thread_id, turn_id)] = _LiveTurn()
                    while len(self._turns) > _MAX_LIVE_TURNS:
                        self._turns.popitem(last=False)
                elif (thread_id, turn_id) in self._turns:
                    self._turns[(thread_id, turn_id)].completed = True
                return
            if method not in {"item/started", "item/completed"}:
                return
            turn_id = params.get("turnId")
            item = params.get("item")
            if (
                not isinstance(turn_id, str)
                or not isinstance(item, Mapping)
                or not isinstance(item.get("id"), str)
            ):
                return
            live = self._turns.get((thread_id, turn_id))
            if live is None:
                return
            if item["id"] not in live.items and len(live.items) >= _MAX_LIVE_ITEMS_PER_TURN:
                return
            live.items[item["id"]] = _bounded_live_value(dict(item))

    def apply(self, thread_id: str, thread: Mapping[str, Any]) -> None:
        """Swap in the observed item list for turns whose live record is complete."""

        turns = thread.get("turns")
        if not isinstance(turns, list):
            return
        with self._lock:
            for turn in turns:
                if not isinstance(turn, dict) or not isinstance(turn.get("id"), str):
                    continue
                live = self._turns.get((thread_id, turn["id"]))
                if live is None or not live.items:
                    continue
                # A finished turn whose completion notification is still queued
                # would lose its latest items here, so keep the stored rollout view.
                if turn.get("status") != "inProgress" and not live.completed:
                    continue
                turn["items"] = [dict(item) for item in live.items.values()]


class _ControllerConnection:
    """Thin, bounded adapter over ``SessionScriptClient``'s controller scope."""

    CLIENT_NAME = "xedoc-remote-agentd"
    CLIENT_TITLE = "Xedoc Remote Agent"
    CLIENT_VERSION = "0.1.0"

    def __init__(self, client: Any) -> None:
        self._client = client
        self._closed = False
        self._client_lock = threading.RLock()
        self._live_items = _LiveTurnItems()
        set_handler = getattr(client, "set_notification_handler", None)
        if callable(set_handler):
            set_handler(self._live_items.observe)

    @classmethod
    def _connect_from_bootstrap(
        cls,
        *,
        xedoc_home: str | Path | None = None,
        timeout: float = 30.0,
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
        response = self._request(
            "thread/read",
            {"threadId": thread_id, "includeTurns": include_turns},
        )
        thread = response.get("thread")
        if include_turns and isinstance(thread, dict):
            self._live_items.apply(thread_id, thread)
        return response

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
        timeout: float = 30.0,
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
