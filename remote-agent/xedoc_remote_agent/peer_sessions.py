"""Scoped, bounded Stage 5 session operations for authenticated peers."""

from __future__ import annotations

from collections import OrderedDict
from collections import deque
from dataclasses import dataclass, field
import json
import secrets
import threading
import time
from typing import Any, Callable, Mapping

from .errors import BrokerError, map_controller_error
from .models import OperationRecord, OperationState
from .peer_session_contract import (
    ITEM_LABELS,
    MAX_ACTIVITY_DELTA_BYTES,
    MAX_RELAY_EVENTS,
    MAX_RELAY_READ_LIMIT,
    MAX_OUTPUT_TEXT_BYTES,
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
_TRANSCRIPT_PAGE_BYTES = 16 * 1024
_MAX_TRANSCRIPT_SNAPSHOTS_PER_PEER = 4


@dataclass
class _StoredOperation:
    peer_host_id: str
    workspace_id: str | None
    record: OperationRecord
    activity_cursor: int = 0
    last_sections: list[str] = field(default_factory=list)


@dataclass
class _Relay:
    workspace_id: str
    peers: set[str] = field(default_factory=set)
    journal: deque["_RelayEvent"] = field(
        default_factory=lambda: deque(maxlen=MAX_RELAY_EVENTS)
    )
    sources: OrderedDict[str, str] = field(default_factory=OrderedDict)
    transcript_snapshots: dict[str, "_TranscriptSnapshot"] = field(default_factory=dict)
    next_sequence: int = 1


@dataclass(frozen=True)
class _TranscriptSnapshot:
    token: str
    peer_host_id: str
    content: bytes


@dataclass(frozen=True)
class _RelayEvent:
    sequence: int
    event_type: str
    turn_id: str
    text: str
    is_delta: bool

    def to_dict(self) -> dict[str, Any]:
        return {
            "cursor": _relay_cursor(self.sequence),
            "type": self.event_type,
            "turnId": self.turn_id,
            "text": self.text,
            "isDelta": self.is_delta,
        }


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
            "_steer_turn",
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
        if operation == "session/read":
            return self._read(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/send":
            return self._send(peer_host_id, params, policy, ensure_authorized)
        if operation == "session/steer":
            return self._steer(peer_host_id, params, policy, ensure_authorized)
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
                relay.transcript_snapshots.pop(peer_host_id, None)
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
                if thread_id in attached:
                    self._complete(
                        stored,
                        thread_id=thread_id,
                        result={"threadId": thread_id, "workspaceId": workspace_id},
                    )
                    return _operation_handle(stored)
                if thread_id in self._detaching:
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
        turn_id: str | None = None
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
        result = _operation_handle(stored)
        if turn_id is not None:
            result.update({"threadId": thread_id, "turnId": turn_id})
        return result

    def _steer(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        turn_id = _identifier(params["turnId"])
        message = params["message"]
        if len(message.encode("utf-8")) > self._max_message_bytes:
            raise BrokerError.limit_exceeded()
        _, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=False
        )
        stored = self._create(peer_host_id, "session/steer", workspace_id)
        try:
            ensure_authorized()
            response = self._controller._steer_turn(
                thread_id,
                turn_id,
                message,
                f"steer_{secrets.token_urlsafe(18)}",
            )
            if _turn_id(response) != turn_id:
                raise BrokerError.conflict()
            with self._lock:
                active = self._active_turns.get(thread_id)
                if active is None:
                    self._active_turns[thread_id] = _ActiveTurn(
                        peer_host_id=peer_host_id,
                        operation_id=stored.record.operation_id,
                        turn_id=turn_id,
                    )
            self._complete(
                stored,
                thread_id=thread_id,
                turn_id=turn_id,
                result={"threadId": thread_id, "turnId": turn_id},
            )
        except BaseException as error:
            self._fail(stored, error)
        return _operation_handle(stored)

    def _read(
        self,
        peer_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        thread_id = _identifier(params["threadId"])
        cursor = params.get("cursor")
        thread, workspace_id = self._resolve_thread(
            thread_id, policy, ensure_authorized, include_turns=True
        )
        with self._lock:
            relay = self._relays.get(thread_id)
            if relay is None or peer_host_id not in relay.peers:
                raise BrokerError.not_found()
            if relay.workspace_id != workspace_id:
                raise BrokerError.internal()
            snapshot, offset = _transcript_snapshot(
                relay,
                peer_host_id,
                thread,
                cursor,
            )
            output_text, next_offset = _transcript_page(
                snapshot.content,
                offset,
                min(_TRANSCRIPT_PAGE_BYTES, self._max_result_bytes // 2),
            )
            if next_offset is None:
                relay.transcript_snapshots.pop(snapshot.token, None)
                next_cursor = None
            else:
                next_cursor = f"transcript_{snapshot.token}_{next_offset}"
        result: dict[str, Any] = {
            "threadId": thread_id,
            "workspaceId": workspace_id,
            "isRunning": _is_running(thread),
            "status": _status_name(thread),
            "nextCursor": next_cursor,
        }
        if output_text:
            result["outputText"] = output_text
        active_turn_id = _active_turn_id(thread)
        if active_turn_id is not None:
            result["activeTurnId"] = active_turn_id
            result["activitySummary"] = "Remote session is running."
        if _encoded_size(result) > self._max_result_bytes:
            raise BrokerError.limit_exceeded()
        return result

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
            elif active_turn_id := _active_turn_id(thread):
                result["activeTurnId"] = active_turn_id
            relay = self._relays.get(thread_id)
            if relay is not None:
                self._sync_relay_locked(relay, thread)
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
        events: list[dict[str, Any]] = []
        last_event: dict[str, Any] | None = None
        while True:
            thread, _ = self._resolve_thread(
                record.thread_id, policy, ensure_authorized, include_turns=True
            )
            with self._lock:
                relay = self._relays.get(record.thread_id)
                if relay is not None:
                    self._sync_relay_locked(relay, thread)
            terminal = _turn_terminal_state(thread, record.turn_id)
            event = (
                _terminal_event_from_name(terminal)
                if terminal is not None
                else _running_event(stored, thread, record.turn_id)
            )
            if terminal is not None:
                self._finish_turn(stored, terminal, thread)
            if event != last_event:
                stop_reason = self._append_event(stored, events, event)
                if stop_reason is not None:
                    return self._wait_result(stored, events, stop_reason)
                last_event = event
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
                relay.transcript_snapshots = {
                    token: snapshot
                    for token, snapshot in relay.transcript_snapshots.items()
                    if snapshot.peer_host_id != peer_host_id
                }
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

    def _sync_relay_locked(self, relay: _Relay, thread: Mapping[str, Any]) -> None:
        for source, event_type, turn_id, text in _relay_sources(thread):
            previous = relay.sources.get(source)
            if previous == text:
                relay.sources.move_to_end(source)
                continue
            is_delta = previous is not None and text.startswith(previous)
            payload = text[len(previous) :] if is_delta and previous is not None else text
            payload = _bounded_output_text(payload, MAX_ACTIVITY_DELTA_BYTES)
            relay.sources[source] = text
            relay.sources.move_to_end(source)
            self._append_relay_event_locked(
                relay,
                event_type,
                turn_id,
                payload,
                is_delta,
            )
        active_turn_id = _active_turn_id(thread)
        if active_turn_id is not None:
            summary = "Remote session is running."
            source = f"activity:{active_turn_id}"
            if relay.sources.get(source) != summary:
                relay.sources[source] = summary
                relay.sources.move_to_end(source)
                self._append_relay_event_locked(
                    relay,
                    "activity",
                    active_turn_id,
                    summary,
                    False,
                )
        while len(relay.sources) > MAX_RELAY_EVENTS * 2:
            relay.sources.popitem(last=False)

    def _append_relay_event_locked(
        self,
        relay: _Relay,
        event_type: str,
        turn_id: str,
        text: str,
        is_delta: bool,
    ) -> None:
        if not text:
            return
        relay.journal.append(
            _RelayEvent(
                sequence=relay.next_sequence,
                event_type=event_type,
                turn_id=turn_id,
                text=text,
                is_delta=is_delta,
            )
        )
        relay.next_sequence += 1

    def _finish_turn(
        self, stored: _StoredOperation, terminal: str, thread: Mapping[str, Any]
    ) -> None:
        record = stored.record
        if record.thread_id is None or record.turn_id is None:
            raise BrokerError.internal()
        with self._lock:
            active = self._active_turns.get(record.thread_id)
            if active is not None and active.operation_id == record.operation_id:
                self._active_turns.pop(record.thread_id, None)
            state = OperationState.FAILED if terminal == "failed" else OperationState.COMPLETED
            error = BrokerError.internal() if terminal == "failed" else None
            result: dict[str, str] = {
                "threadId": record.thread_id,
                "turnId": record.turn_id,
                "status": terminal,
            }
            output_text = _terminal_output(
                thread, record.turn_id, min(self._max_result_bytes, MAX_OUTPUT_TEXT_BYTES)
            )
            if output_text:
                result["outputText"] = output_text
            self._set_record_locked(
                stored,
                state,
                result=result,
                error=error,
            )

    def _append_event(
        self,
        stored: _StoredOperation,
        events: list[dict[str, Any]],
        event: dict[str, Any],
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
        events: list[dict[str, Any]],
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
    if not isinstance(response, Mapping):
        raise BrokerError.internal()
    direct_turn_id = response.get("turnId")
    if direct_turn_id is not None:
        return _identifier(direct_turn_id)
    turn = response.get("turn")
    if not isinstance(turn, Mapping):
        raise BrokerError.internal()
    return _identifier(turn.get("id", turn.get("turnId")))


def _active_turn_id(thread: Mapping[str, Any]) -> str | None:
    turns = thread.get("turns")
    if not isinstance(turns, list):
        return None
    for turn in reversed(turns):
        if not isinstance(turn, Mapping):
            continue
        status = turn.get("status")
        status = status.get("type") if isinstance(status, Mapping) else status
        if status in {"active", "inProgress", "running"}:
            return _identifier(turn.get("id", turn.get("turnId")))
    return None


def _relay_cursor(sequence: int) -> str:
    if not isinstance(sequence, int) or sequence < 0:
        raise BrokerError.internal()
    return f"relay_{sequence}"


def _transcript_snapshot(
    relay: _Relay,
    peer_host_id: str,
    thread: Mapping[str, Any],
    cursor: Any,
) -> tuple[_TranscriptSnapshot, int]:
    if cursor is None:
        snapshot_count = sum(
            snapshot.peer_host_id == peer_host_id
            for snapshot in relay.transcript_snapshots.values()
        )
        if snapshot_count >= _MAX_TRANSCRIPT_SNAPSHOTS_PER_PEER:
            raise BrokerError.limit_exceeded()
        snapshot = _TranscriptSnapshot(
            token=secrets.token_hex(16),
            peer_host_id=peer_host_id,
            content=_transcript_text(thread).encode("utf-8"),
        )
        relay.transcript_snapshots[snapshot.token] = snapshot
        return snapshot, 0
    token, offset = _transcript_cursor(cursor)
    snapshot = relay.transcript_snapshots.get(token)
    if snapshot is None or not secrets.compare_digest(snapshot.peer_host_id, peer_host_id):
        raise BrokerError.not_found()
    return snapshot, offset


def _transcript_page(content: bytes, offset: int, max_bytes: int) -> tuple[str, int | None]:
    if max_bytes < 1:
        raise BrokerError.limit_exceeded()
    if offset > len(content):
        raise BrokerError.invalid_request()
    if offset == len(content):
        return "", None
    end = min(offset + max_bytes, len(content))
    while end > offset:
        try:
            page = content[offset:end].decode("utf-8")
            break
        except UnicodeDecodeError:
            end -= 1
    else:
        raise BrokerError.internal()
    if end >= len(content):
        return page, None
    return page, end


def _transcript_cursor(cursor: Any) -> tuple[str, int]:
    cursor = _identifier(cursor)
    prefix = "transcript_"
    value = cursor.removeprefix(prefix)
    if value == cursor:
        raise BrokerError.invalid_request()
    token, separator, offset = value.rpartition("_")
    if not separator or not token or not offset.isdecimal():
        raise BrokerError.invalid_request()
    return token, int(offset)


def _transcript_text(thread: Mapping[str, Any]) -> str:
    parts: list[str] = []
    for _, event_type, _, text in _transcript_sources(thread):
        parts.append(_section(event_type, text))
    return "\n\n".join(parts)


def _relay_page(
    relay: _Relay, cursor: Any, limit: int
) -> tuple[list[dict[str, Any]], str | None]:
    after = _relay_sequence(cursor)
    if limit < 1 or limit > MAX_RELAY_READ_LIMIT:
        raise BrokerError.invalid_request()
    available = [event for event in relay.journal if event.sequence > after]
    events = [event.to_dict() for event in available[:limit]]
    if events:
        return events, events[-1]["cursor"]
    return events, _relay_cursor(after) if after else None


def _relay_sequence(cursor: Any) -> int:
    if cursor is None:
        return 0
    cursor = _identifier(cursor)
    prefix = "relay_"
    sequence = cursor.removeprefix(prefix)
    if sequence == cursor or not sequence.isdecimal():
        raise BrokerError.invalid_request()
    return int(sequence)


def _relay_sources(
    thread: Mapping[str, Any],
) -> list[tuple[str, str, str, str]]:
    return _transcript_sources(thread)[-MAX_RELAY_EVENTS:]


def _transcript_sources(
    thread: Mapping[str, Any],
) -> list[tuple[str, str, str, str]]:
    turns = thread.get("turns")
    if not isinstance(turns, list):
        return []
    sources: list[tuple[str, str, str, str]] = []
    for turn in turns:
        if not isinstance(turn, Mapping):
            continue
        try:
            turn_id = _identifier(turn.get("id", turn.get("turnId")))
        except BrokerError:
            continue
        items = turn.get("items")
        if not isinstance(items, list):
            continue
        for index, item in enumerate(items):
            if not isinstance(item, Mapping):
                continue
            event_type, text = _relay_item(item)
            if event_type is None or text is None:
                continue
            sources.append((f"{turn_id}:{index}:{event_type}", event_type, turn_id, text))
    return sources


def _relay_item(item: Mapping[str, Any]) -> tuple[str | None, str | None]:
    item_type = item.get("type")
    if item_type not in ITEM_LABELS:
        return None, None
    text = _RENDERERS[item_type](item)
    if not isinstance(text, str) or not text.strip():
        return None, None
    return item_type, text


def _section(item_type: str, text: str) -> str:
    return f"{ITEM_LABELS[item_type]}:\n{text}"


def _user_message_text(item: Mapping[str, Any]) -> str:
    content = item.get("content")
    if not isinstance(content, list):
        text = item.get("text")
        return text if isinstance(text, str) else ""
    parts: list[str] = []
    for part in content:
        if not isinstance(part, Mapping):
            continue
        if part.get("type") == "text" and isinstance(part.get("text"), str):
            parts.append(part["text"])
        elif part.get("type") in {"image", "localImage"}:
            parts.append("[image]")
    return "\n".join(parts)


def _text_field(item: Mapping[str, Any]) -> str:
    text = item.get("text")
    return text if isinstance(text, str) else ""


def _reasoning_text(item: Mapping[str, Any]) -> str:
    for key in ("summary", "content"):
        values = item.get(key)
        if isinstance(values, list):
            text = "\n".join(value for value in values if isinstance(value, str))
            if text.strip():
                return text
    return ""


def _status_suffix(item: Mapping[str, Any]) -> str:
    status = item.get("status")
    return f" ({status})" if status in {"failed", "declined"} else ""


def _command_text(item: Mapping[str, Any]) -> str:
    command = item.get("command")
    if not isinstance(command, str) or not command:
        return ""
    text = f"$ {command}{_status_suffix(item)}"
    output = item.get("aggregatedOutput")
    if isinstance(output, str) and output:
        text = f"{text}\n{output}"
    return text


def _file_change_text(item: Mapping[str, Any]) -> str:
    changes = item.get("changes")
    if not isinstance(changes, list) or not changes:
        return ""
    lines = []
    for change in changes[:_MAX_LISTED_ITEMS]:
        if not isinstance(change, Mapping) or not isinstance(change.get("path"), str):
            continue
        kind = change.get("kind")
        kind = kind.get("type") if isinstance(kind, Mapping) else kind
        diff = change.get("diff")
        added = removed = 0
        if isinstance(diff, str) and kind == "add":
            added = len(diff.splitlines())
        elif isinstance(diff, str) and kind == "delete":
            removed = len(diff.splitlines())
        elif isinstance(diff, str):
            for line in diff.splitlines():
                if line.startswith("+") and not line.startswith("+++"):
                    added += 1
                elif line.startswith("-") and not line.startswith("---"):
                    removed += 1
        lines.append(f"{kind or 'update'} {change['path']} (+{added} -{removed})")
    if len(changes) > _MAX_LISTED_ITEMS:
        lines.append(f"… {len(changes) - _MAX_LISTED_ITEMS} more")
    return f"{len(changes)} file(s){_status_suffix(item)}\n" + "\n".join(lines)


def _json_snippet(value: Any) -> str:
    try:
        text = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    except (TypeError, ValueError):
        return ""
    return text if len(text) <= _MAX_ARGUMENT_CHARS else text[:_MAX_ARGUMENT_CHARS] + "…"


def _content_text(items: Any) -> str:
    if not isinstance(items, list):
        return ""
    return "\n".join(
        part["text"]
        for part in items
        if isinstance(part, Mapping) and isinstance(part.get("text"), str)
    )


def _mcp_tool_text(item: Mapping[str, Any]) -> str:
    name = f"{item.get('server')}/{item.get('tool')}"
    text = f"{name} {_json_snippet(item.get('arguments'))}{_status_suffix(item)}"
    error = item.get("error")
    result = item.get("result")
    detail = (
        error.get("message")
        if isinstance(error, Mapping)
        else _content_text(result.get("content") if isinstance(result, Mapping) else None)
    )
    if isinstance(detail, str) and detail:
        text = f"{text}\n{detail}"
    return text


def _dynamic_tool_text(item: Mapping[str, Any]) -> str:
    namespace = item.get("namespace")
    name = f"{namespace}/{item.get('tool')}" if namespace else str(item.get("tool"))
    text = f"{name} {_json_snippet(item.get('arguments'))}{_status_suffix(item)}"
    detail = _content_text(item.get("contentItems"))
    return f"{text}\n{detail}" if detail else text


def _collab_agent_text(item: Mapping[str, Any]) -> str:
    prompt = item.get("prompt")
    text = f"{item.get('tool')}{_status_suffix(item)}"
    return f"{text}\n{prompt}" if isinstance(prompt, str) and prompt else text


_MAX_LISTED_ITEMS = 16
_MAX_ARGUMENT_CHARS = 300
_RENDERERS: dict[str, Callable[[Mapping[str, Any]], str]] = {
    "userMessage": _user_message_text,
    "agentMessage": _text_field,
    "plan": _text_field,
    "reasoning": _reasoning_text,
    "commandExecution": _command_text,
    "fileChange": _file_change_text,
    "mcpToolCall": _mcp_tool_text,
    "dynamicToolCall": _dynamic_tool_text,
    "collabAgentToolCall": _collab_agent_text,
}


def _status_name(thread: Mapping[str, Any]) -> str:
    status = thread.get("status")
    value = status.get("type") if isinstance(status, Mapping) else status
    if not isinstance(value, str) or len(value) > 64:
        return "unknown"
    if value in {"interrupted", "aborted"}:
        return "cancelled"
    if value in {"error", "systemError"}:
        return "failed"
    return value


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
    return None


def _terminal_output(
    thread: Mapping[str, Any], turn_id: str, max_bytes: int
) -> str:
    if max_bytes <= 0:
        return ""
    return _bounded_output_text("\n\n".join(_turn_sections(thread, turn_id)), max_bytes)


def _bounded_output_text(value: str, max_bytes: int) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= max_bytes:
        return value
    suffix = "…".encode("utf-8")
    return (
        encoded[: max(0, max_bytes - len(suffix))].decode("utf-8", "ignore") + "…"
    )


def _running_event(
    stored: _StoredOperation, thread: Mapping[str, Any], turn_id: str
) -> dict[str, str]:
    event = {
        "type": "progress",
        "status": "running",
        "activitySummary": "Remote session is running.",
    }
    sections = _turn_sections(thread, turn_id, _MAX_SECTION_BYTES)
    delta = _sections_delta(stored.last_sections, sections)
    if not delta:
        return event
    stored.last_sections = sections
    stored.activity_cursor += 1
    event["outputDelta"] = _bounded_output_tail(delta, MAX_ACTIVITY_DELTA_BYTES)
    event["outputCursor"] = f"activity_{stored.activity_cursor}"
    return event


_MAX_SECTION_BYTES = 16 * 1024


def _turn_sections(
    thread: Mapping[str, Any], turn_id: str, max_section_bytes: int | None = None
) -> list[str]:
    """Render every visible item of one turn, except the user's own input, as labeled sections."""

    turns = thread.get("turns")
    if not isinstance(turns, list):
        return []
    for turn in reversed(turns):
        if not isinstance(turn, Mapping) or turn.get("id", turn.get("turnId")) != turn_id:
            continue
        items = turn.get("items")
        if not isinstance(items, list):
            return []
        sections: list[str] = []
        for item in items:
            if not isinstance(item, Mapping) or item.get("type") == "userMessage":
                continue
            item_type, text = _relay_item(item)
            if item_type is None or text is None:
                continue
            if max_section_bytes is not None:
                text = _bounded_output_text(text, max_section_bytes)
            sections.append(_section(item_type, text))
        return sections
    return []


def _sections_delta(previous: list[str], current: list[str]) -> str:
    """Return what changed between two renderings, appending to grown sections in place."""

    parts: list[str] = []
    for index, section in enumerate(current):
        old = previous[index] if index < len(previous) else None
        if old == section:
            continue
        if old is not None and section.startswith(old):
            parts.append(section[len(old) :])
        else:
            parts.append(f"\n{section}")
    return "\n".join(parts).strip("\n")


def _bounded_output_tail(value: str, max_bytes: int) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= max_bytes:
        return value
    prefix = "…".encode("utf-8")
    return prefix.decode() + encoded[-(max_bytes - len(prefix)) :].decode(
        "utf-8", "ignore"
    )


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
