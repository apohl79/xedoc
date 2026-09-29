"""Scoped, bounded Stage 5 session operations for authenticated peers."""

from __future__ import annotations

from collections import OrderedDict
from dataclasses import dataclass, field
import secrets
import threading
import time
from typing import Any, Callable, Mapping

from .errors import BrokerError, map_controller_error
from .models import OperationRecord, OperationState
from .peer_session_contract import (
    MAX_RELAY_EVENTS,
    encoded_size as _encoded_size,
    identifier as _identifier,
    validate_session_params,
)
from .peer_state import GrantPolicy
from .workspaces import WorkspaceRegistry


_MAX_OPERATION_RECORDS = 8_192
_OPERATION_TTL_SECONDS = 300
_POLL_INTERVAL_SECONDS = 0.1
_MIN_RESULT_BYTES = 1024


@dataclass
class _StoredOperation:
    peer_host_id: str
    workspace_id: str | None
    record: OperationRecord


@dataclass
class _Relay:
    workspace_id: str
    peers: set[str] = field(default_factory=set)


@dataclass(frozen=True)
class _ActiveTurn:
    peer_host_id: str
    operation_id: str
    turn_id: str


class PeerSessionOperations:
    """Own peer-scoped handles, turns, and controller subscription relays."""

    def __init__(
        self,
        controller: Any,
        workspaces: WorkspaceRegistry,
        *,
        max_attached_sessions: int,
        max_message_bytes: int,
        max_result_bytes: int,
        max_wait_seconds: int,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        if not isinstance(workspaces, WorkspaceRegistry):
            raise BrokerError.invalid_request()
        required = (
            "_start_session",
            "_resume_session",
            "_attach_session",
            "_read_session",
            "_start_turn",
            "_interrupt_turn",
            "_unsubscribe",
        )
        if any(not callable(getattr(controller, name, None)) for name in required):
            raise BrokerError.invalid_request()
        limits = (
            max_attached_sessions,
            max_message_bytes,
            max_wait_seconds,
        )
        if any(
            not isinstance(value, int) or isinstance(value, bool) or value <= 0
            for value in limits
        ):
            raise BrokerError.invalid_request()
        if (
            not isinstance(max_result_bytes, int)
            or isinstance(max_result_bytes, bool)
            or max_result_bytes < _MIN_RESULT_BYTES
        ):
            raise BrokerError.invalid_request()
        self._controller = controller
        self._workspaces = workspaces
        self._max_attached_sessions = max_attached_sessions
        self._max_message_bytes = max_message_bytes
        self._max_result_bytes = max_result_bytes
        self._max_wait_seconds = max_wait_seconds
        self._clock = clock
        self._records: OrderedDict[str, _StoredOperation] = OrderedDict()
        self._relays: dict[str, _Relay] = {}
        self._peer_attachments: dict[str, set[str]] = {}
        self._active_turns: dict[str, _ActiveTurn] = {}
        self._subscribing: set[str] = set()
        self._detaching: set[str] = set()
        self._turn_starting: set[str] = set()
        self._lock = threading.RLock()

    def handle(
        self,
        peer_host_id: str,
        operation: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        """Execute one fixed operation after peer admission has created a policy."""

        peer_host_id = _identifier(peer_host_id)
        if not isinstance(policy, GrantPolicy) or not callable(ensure_authorized):
            raise BrokerError.invalid_request()
        params = validate_session_params(operation, params)
        if operation == "session/start":
            return self._start(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/resume":
            return self._resume(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/attach":
            return self._attach(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/send":
            return self._send(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/status":
            return self._status(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/wait":
            return self._wait(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/cancel":
            return self._cancel(peer_host_id, params, policy, ensure_authorized)
        return self._detach(peer_host_id, params, policy, ensure_authorized)

    def detach_peer(self, peer_host_id: str) -> None:
        """Detach one relationship's relays without ever interrupting a turn."""

        peer_host_id = _identifier(peer_host_id)
        with self._lock:
            self._expire_locked()
            thread_ids = tuple(self._peer_attachments.pop(peer_host_id, set()))
            for stored in self._records.values():
                if (
                    stored.peer_host_id == peer_host_id
                    and not stored.record.state.terminal
                ):
                    self._set_record_locked(
                        stored,
                        OperationState.EXPIRED,
                        error=BrokerError.unavailable(),
                    )
        for thread_id in thread_ids:
            unsubscribe = False
            with self._lock:
                relay = self._relays.get(thread_id)
                if relay is None:
                    continue
                relay.peers.discard(peer_host_id)
                if relay.peers:
                    continue
                self._relays.pop(thread_id, None)
                self._detaching.add(thread_id)
                unsubscribe = True
            if unsubscribe:
                try:
                    self._controller._unsubscribe(thread_id)
                except BaseException:
                    pass
                finally:
                    with self._lock:
                        self._detaching.discard(thread_id)
        with self._lock:
            for thread_id, active in tuple(self._active_turns.items()):
                if active.peer_host_id == peer_host_id:
                    self._active_turns.pop(thread_id, None)

    def _start(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        workspace_id = _identifier(params["workspaceId"])
        if not policy.permits_start(workspace_id):
            raise BrokerError.unauthorized()
        cwd = self._workspaces.resolve_request(params)
        stored = self._create(peer_host_id, "session/start", workspace_id)
        try:
            ensure_authorized()
            thread = _thread(self._controller._start_session(str(cwd)))
            thread_id = _thread_id(thread)
            if self._workspace_id(thread) != workspace_id:
                raise BrokerError.unauthorized()
            self._controller._unsubscribe(thread_id)
            self._complete(
                stored,
                thread_id=thread_id,
                result={"threadId": thread_id, "workspaceId": workspace_id},
            )
        except BaseException as error:
            self._fail(stored, error)
        return _operation_handle(stored)

    def _resume(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        _, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=False
        )
        with self._lock:
            if thread_id in self._relays or thread_id in self._subscribing:
                raise BrokerError.conflict()
        stored = self._create(peer_host_id, "session/resume", workspace_id)
        try:
            ensure_authorized()
            thread = _thread(self._controller._resume_session(thread_id))
            if _thread_id(thread) != thread_id or self._workspace_id(thread) != workspace_id:
                raise BrokerError.unauthorized()
            self._controller._unsubscribe(thread_id)
            self._complete(
                stored,
                thread_id=thread_id,
                result={"threadId": thread_id, "workspaceId": workspace_id},
            )
        except BaseException as error:
            self._fail(stored, error)
        return _operation_handle(stored)

    def _attach(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        _, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=False
        )
        stored = self._create(peer_host_id, "session/attach", workspace_id)
        subscribe = False
        subscribed = False
        try:
            with self._lock:
                attached = self._peer_attachments.setdefault(peer_host_id, set())
                if thread_id in attached or thread_id in self._detaching:
                    raise BrokerError.conflict()
                if (
                    len(attached) >= self._max_attached_sessions
                    or self._attachment_count_locked() >= self._max_attached_sessions
                ):
                    raise BrokerError.limit_exceeded()
                if thread_id in self._subscribing:
                    raise BrokerError.conflict()
                subscribe = thread_id not in self._relays
                if subscribe:
                    self._subscribing.add(thread_id)
            if subscribe:
                ensure_authorized()
                thread = _thread(self._controller._attach_session(thread_id))
                subscribed = True
                if (
                    _thread_id(thread) != thread_id
                    or self._workspace_id(thread) != workspace_id
                ):
                    raise BrokerError.unauthorized()
            with self._lock:
                relay = self._relays.setdefault(thread_id, _Relay(workspace_id))
                if relay.workspace_id != workspace_id:
                    raise BrokerError.internal()
                if self._attachment_count_locked() >= self._max_attached_sessions:
                    raise BrokerError.limit_exceeded()
                relay.peers.add(peer_host_id)
                self._peer_attachments.setdefault(peer_host_id, set()).add(thread_id)
                self._subscribing.discard(thread_id)
            self._complete(
                stored,
                thread_id=thread_id,
                result={"threadId": thread_id, "workspaceId": workspace_id},
            )
        except BaseException as error:
            with self._lock:
                self._subscribing.discard(thread_id)
                if not self._peer_attachments.get(peer_host_id):
                    self._peer_attachments.pop(peer_host_id, None)
                unsubscribe = subscribed and thread_id not in self._relays
            if unsubscribe:
                try:
                    self._controller._unsubscribe(thread_id)
                except BaseException:
                    pass
            self._fail(stored, error)
        return _operation_handle(stored)

    def _send(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        message = params["message"]
        if len(message.encode("utf-8")) > self._max_message_bytes:
            raise BrokerError.limit_exceeded()
        thread, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=False
        )
        if _is_running(thread):
            raise BrokerError.conflict()
        stored = self._create(peer_host_id, "session/send", workspace_id)
        try:
            with self._lock:
                if thread_id in self._active_turns or thread_id in self._turn_starting:
                    raise BrokerError.conflict()
                self._turn_starting.add(thread_id)
            ensure_authorized()
            response = self._controller._start_turn(thread_id, message)
            turn_id = _turn_id(response)
            with self._lock:
                self._turn_starting.discard(thread_id)
                self._active_turns[thread_id] = _ActiveTurn(
                    peer_host_id=peer_host_id,
                    operation_id=stored.record.operation_id,
                    turn_id=turn_id,
                )
                self._set_record_locked(
                    stored,
                    OperationState.RUNNING,
                    thread_id=thread_id,
                    turn_id=turn_id,
                    result={"threadId": thread_id, "turnId": turn_id},
                )
        except BaseException as error:
            with self._lock:
                self._turn_starting.discard(thread_id)
            self._fail(stored, error)
        return _operation_handle(stored)

    def _status(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        thread, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=True
        )
        result: dict[str, Any] = {
            "threadId": thread_id,
            "workspaceId": workspace_id,
            "isRunning": _is_running(thread),
            "status": _status_name(thread),
        }
        with self._lock:
            active = self._active_turns.get(thread_id)
            if not result["isRunning"]:
                self._active_turns.pop(thread_id, None)
            elif active is not None and active.peer_host_id == peer_host_id:
                result["activeTurnId"] = active.turn_id
        if _encoded_size(result) > self._max_result_bytes:
            raise BrokerError.limit_exceeded()
        return result

    def _wait(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        operation_id = _identifier(params["operationId"])
        timeout = float(params["timeoutSeconds"])
        if timeout > self._max_wait_seconds:
            raise BrokerError.limit_exceeded()
        stored = self._record(peer_host_id, operation_id)
        record = stored.record
        if record.thread_id is None:
            return self._wait_result(stored, [], "terminal")
        self._resolve_thread(
            record.thread_id, policy, ensure_authorized, include_turns=False
        )
        if record.state.terminal:
            return self._wait_result(stored, [_terminal_event(record.state)], "terminal")
        if record.operation != "session/send" or record.turn_id is None:
            return self._wait_result(stored, [], "timeout")

        deadline = self._clock() + timeout
        events: list[dict[str, str]] = []
        last_event: tuple[str, str] | None = None
        while True:
            thread, _ = self._resolve_thread(
                record.thread_id, policy, ensure_authorized, include_turns=True
            )
            terminal = _turn_terminal_state(thread, record.turn_id)
            event = (
                _terminal_event_from_name(terminal)
                if terminal is not None
                else {"type": "progress", "status": "running"}
            )
            if terminal is not None:
                self._finish_turn(stored, terminal)
            marker = (event["type"], event["status"])
            if marker != last_event:
                stop_reason = self._append_event(stored, events, event)
                if stop_reason is not None:
                    return self._wait_result(stored, events, stop_reason)
                last_event = marker
            if terminal is not None:
                return self._wait_result(stored, events, "terminal")
            remaining = deadline - self._clock()
            if remaining <= 0:
                return self._wait_result(stored, events, "timeout")
            time.sleep(min(_POLL_INTERVAL_SECONDS, remaining))

    def _cancel(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        turn_id = _identifier(params["turnId"])
        _, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=False
        )
        with self._lock:
            active = self._active_turns.get(thread_id)
            if (
                active is None
                or active.peer_host_id != peer_host_id
                or active.turn_id != turn_id
            ):
                raise BrokerError.conflict()
        stored = self._create(peer_host_id, "session/cancel", workspace_id)
        try:
            ensure_authorized()
            self._controller._interrupt_turn(thread_id, turn_id)
            with self._lock:
                active = self._active_turns.pop(thread_id, None)
                if active is not None:
                    source = self._records.get(active.operation_id)
                    if source is not None:
                        self._set_record_locked(
                            source,
                            OperationState.CANCELLED,
                            result={"threadId": thread_id, "turnId": turn_id, "status": "cancelled"},
                        )
                self._set_record_locked(
                    stored,
                    OperationState.CANCELLED,
                    thread_id=thread_id,
                    turn_id=turn_id,
                    result={"threadId": thread_id, "turnId": turn_id},
                )
        except BaseException as error:
            self._fail(stored, error)
        return _operation_handle(stored)

    def _detach(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        _, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=False
        )
        stored = self._create(peer_host_id, "session/detach", workspace_id)
        try:
            with self._lock:
                relay = self._relays.get(thread_id)
                if relay is None or peer_host_id not in relay.peers:
                    raise BrokerError.not_found()
                if thread_id in self._detaching:
                    raise BrokerError.conflict()
                unsubscribe = relay.peers == {peer_host_id}
                if unsubscribe:
                    self._detaching.add(thread_id)
            if unsubscribe:
                ensure_authorized()
                self._controller._unsubscribe(thread_id)
            with self._lock:
                relay = self._relays.get(thread_id)
                if relay is None:
                    raise BrokerError.internal()
                relay.peers.remove(peer_host_id)
                self._peer_attachments.get(peer_host_id, set()).discard(thread_id)
                if not self._peer_attachments.get(peer_host_id):
                    self._peer_attachments.pop(peer_host_id, None)
                if not relay.peers:
                    self._relays.pop(thread_id, None)
                self._detaching.discard(thread_id)
                self._set_record_locked(
                    stored,
                    OperationState.COMPLETED,
                    thread_id=thread_id,
                    result={"threadId": thread_id},
                )
        except BaseException as error:
            with self._lock:
                self._detaching.discard(thread_id)
            self._fail(stored, error)
        return _operation_handle(stored)

    def _resolve_thread(
        self,
        thread_id: str,
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
        *,
        include_turns: bool,
    ) -> tuple[Mapping[str, Any], str]:
        if not policy.permits_thread(thread_id):
            raise BrokerError.unauthorized()
        ensure_authorized()
        thread = _thread(
            self._controller._read_session(thread_id, include_turns=include_turns)
        )
        if _thread_id(thread) != thread_id:
            raise BrokerError.internal()
        workspace_id = self._workspace_id(thread)
        if not policy.permits_session(workspace_id, thread_id):
            raise BrokerError.unauthorized()
        return thread, workspace_id

    def _workspace_id(self, thread: Mapping[str, Any]) -> str:
        cwd = thread.get("cwd")
        if not isinstance(cwd, str) or not cwd:
            raise BrokerError.unauthorized()
        workspace = self._workspaces.workspace_for_path(cwd)
        if workspace is None:
            raise BrokerError.unauthorized()
        return workspace.workspace_id

    def _create(
        self, peer_host_id: str, operation: str, workspace_id: str | None
    ) -> _StoredOperation:
        with self._lock:
            self._expire_locked()
            while len(self._records) >= _MAX_OPERATION_RECORDS:
                discarded = next(
                    (
                        operation_id
                        for operation_id, stored in self._records.items()
                        if stored.record.state.terminal
                    ),
                    None,
                )
                if discarded is None:
                    raise BrokerError.limit_exceeded()
                del self._records[discarded]
            now = self._clock()
            record = OperationRecord(
                operation_id=f"op_{secrets.token_urlsafe(24)}",
                operation=operation,
                created_at=now,
                updated_at=now,
            )
            stored = _StoredOperation(peer_host_id, workspace_id, record)
            self._records[record.operation_id] = stored
            self._set_record_locked(stored, OperationState.RUNNING)
            return stored

    def _record(self, peer_host_id: str, operation_id: str) -> _StoredOperation:
        with self._lock:
            self._expire_locked()
            stored = self._records.get(operation_id)
            if stored is None or not secrets.compare_digest(
                stored.peer_host_id, peer_host_id
            ):
                raise BrokerError.not_found()
            self._records.move_to_end(operation_id)
            return stored

    def _complete(
        self,
        stored: _StoredOperation,
        *,
        thread_id: str | None = None,
        turn_id: str | None = None,
        result: Mapping[str, Any] | None = None,
    ) -> None:
        with self._lock:
            self._set_record_locked(
                stored,
                OperationState.COMPLETED,
                thread_id=thread_id,
                turn_id=turn_id,
                result=result,
            )

    def _fail(self, stored: _StoredOperation, error: BaseException) -> None:
        with self._lock:
            self._set_record_locked(
                stored,
                OperationState.FAILED,
                error=map_controller_error(error),
            )

    def _set_record_locked(
        self,
        stored: _StoredOperation,
        state: OperationState,
        *,
        thread_id: str | None = None,
        turn_id: str | None = None,
        result: Mapping[str, Any] | None = None,
        error: BrokerError | None = None,
    ) -> None:
        record = stored.record
        if thread_id is not None:
            record.thread_id = _identifier(thread_id)
        if turn_id is not None:
            record.turn_id = _identifier(turn_id)
        record.state = state
        record.result = dict(result) if result is not None else None
        record.error = error
        record.updated_at = self._clock()
        if _encoded_size(record.to_dict()) <= self._max_result_bytes:
            return
        record.state = OperationState.FAILED
        record.result = None
        record.error = BrokerError.limit_exceeded()
        if _encoded_size(record.to_dict()) > self._max_result_bytes:
            raise BrokerError.internal()

    def _expire_locked(self) -> None:
        now = self._clock()
        for stored in self._records.values():
            record = stored.record
            if (
                not record.state.terminal
                and now - record.updated_at >= _OPERATION_TTL_SECONDS
            ):
                self._set_record_locked(
                    stored, OperationState.EXPIRED, error=BrokerError.unavailable()
                )
                active = self._active_turns.get(record.thread_id or "")
                if active is not None and active.operation_id == record.operation_id:
                    self._active_turns.pop(record.thread_id or "", None)

    def _attachment_count_locked(self) -> int:
        return sum(len(attachments) for attachments in self._peer_attachments.values())

    def _finish_turn(self, stored: _StoredOperation, terminal: str) -> None:
        record = stored.record
        if record.thread_id is None or record.turn_id is None:
            raise BrokerError.internal()
        with self._lock:
            active = self._active_turns.get(record.thread_id)
            if active is not None and active.operation_id == record.operation_id:
                self._active_turns.pop(record.thread_id, None)
            state = OperationState.FAILED if terminal == "failed" else OperationState.COMPLETED
            error = BrokerError.internal() if terminal == "failed" else None
            self._set_record_locked(
                stored,
                state,
                result={
                    "threadId": record.thread_id,
                    "turnId": record.turn_id,
                    "status": terminal,
                },
                error=error,
            )

    def _append_event(
        self,
        stored: _StoredOperation,
        events: list[dict[str, str]],
        event: dict[str, str],
    ) -> str | None:
        if len(events) >= MAX_RELAY_EVENTS:
            return "itemLimit"
        candidate = [*events, event]
        with self._lock:
            result = stored.record.to_dict()
        result["events"] = candidate
        result["stopReason"] = "running"
        if _encoded_size(result) > self._max_result_bytes:
            return "byteLimit"
        events.append(event)
        return None

    def _wait_result(
        self,
        stored: _StoredOperation,
        events: list[dict[str, str]],
        stop_reason: str,
    ) -> dict[str, Any]:
        with self._lock:
            result = stored.record.to_dict()
        result["events"] = [dict(event) for event in events]
        result["stopReason"] = stop_reason
        while _encoded_size(result) > self._max_result_bytes and result["events"]:
            result["events"].pop()
            result["stopReason"] = "byteLimit"
        if _encoded_size(result) > self._max_result_bytes:
            raise BrokerError.limit_exceeded()
        return result


def _operation_handle(stored: _StoredOperation) -> dict[str, str]:
    return {"operationId": stored.record.operation_id}


def _thread(response: Any) -> Mapping[str, Any]:
    if not isinstance(response, Mapping) or not isinstance(response.get("thread"), Mapping):
        raise BrokerError.internal()
    return response["thread"]


def _thread_id(thread: Mapping[str, Any]) -> str:
    return _identifier(thread.get("id", thread.get("threadId")))


def _turn_id(response: Any) -> str:
    turn = response.get("turn") if isinstance(response, Mapping) else None
    if not isinstance(turn, Mapping):
        raise BrokerError.internal()
    return _identifier(turn.get("id", turn.get("turnId")))


def _status_name(thread: Mapping[str, Any]) -> str:
    status = thread.get("status")
    value = status.get("type") if isinstance(status, Mapping) else status
    return value if isinstance(value, str) and len(value) <= 64 else "unknown"


def _is_running(thread: Mapping[str, Any]) -> bool:
    running = thread.get("isRunning")
    return running if isinstance(running, bool) else _status_name(thread) in {
        "active",
        "inProgress",
    }


def _turn_terminal_state(thread: Mapping[str, Any], turn_id: str) -> str | None:
    turns = thread.get("turns")
    if isinstance(turns, list):
        for turn in reversed(turns):
            if not isinstance(turn, Mapping):
                continue
            candidate = turn.get("id", turn.get("turnId"))
            if candidate != turn_id:
                continue
            status = turn.get("status")
            status = status.get("type") if isinstance(status, Mapping) else status
            if status in {"active", "inProgress", "running"}:
                return None
            if status in {"cancelled", "interrupted", "aborted"}:
                return "cancelled"
            if status in {"failed", "error"}:
                return "failed"
            return "completed"
    return "completed" if not _is_running(thread) else None


def _terminal_event(state: OperationState) -> dict[str, str]:
    if state is OperationState.CANCELLED:
        return _terminal_event_from_name("cancelled")
    if state is OperationState.FAILED:
        return _terminal_event_from_name("failed")
    if state is OperationState.EXPIRED:
        return _terminal_event_from_name("expired")
    return _terminal_event_from_name("completed")


def _terminal_event_from_name(status: str) -> dict[str, str]:
    return {"type": "terminal", "status": status}
