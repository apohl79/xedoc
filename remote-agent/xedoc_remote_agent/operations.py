"""Bounded local session operations over the private controller adapter."""

from __future__ import annotations

from collections import OrderedDict
import json
from pathlib import Path
import secrets
import threading
import time
from typing import Any, Callable, Mapping

from .errors import BrokerError, map_controller_error
from .models import (
    MAX_ID_LENGTH,
    OperationHandle,
    OperationRecord,
    OperationState,
)
from .workspaces import WorkspaceRegistry


_MAX_OPERATION_RECORDS = 4096
_OPERATION_TTL_SECONDS = 3600.0
_MAX_TIMEOUT_PRECISION = 3
_MIN_RESULT_BYTES = 256


class _OperationStore:
    """Finite in-memory operation state scoped to one broker process."""

    def __init__(
        self,
        *,
        max_records: int,
        max_result_bytes: int,
        clock: Callable[[], float],
    ) -> None:
        if not 0 < max_records <= _MAX_OPERATION_RECORDS or max_result_bytes <= 0:
            raise BrokerError.invalid_request()
        self._max_records = max_records
        self._max_result_bytes = max_result_bytes
        self._clock = clock
        self._records: OrderedDict[str, tuple[str, OperationRecord]] = OrderedDict()
        self._condition = threading.Condition()

    def create(self, entity_id: str, operation: str) -> OperationRecord:
        with self._condition:
            self._expire()
            while len(self._records) >= self._max_records:
                operation_id, (_, record) = next(iter(self._records.items()))
                if not record.state.terminal:
                    raise BrokerError.limit_exceeded()
                del self._records[operation_id]
            now = self._clock()
            record = OperationRecord(
                operation_id=f"op_{secrets.token_urlsafe(24)}",
                operation=operation,
                created_at=now,
                updated_at=now,
            )
            self._records[record.operation_id] = (entity_id, record)
            return record

    def transition(
        self,
        record: OperationRecord,
        state: OperationState,
        *,
        thread_id: str | None = None,
        turn_id: str | None = None,
        result: Mapping[str, Any] | None = None,
        error: BrokerError | None = None,
    ) -> None:
        allowed = {
            OperationState.ACCEPTED: {OperationState.RUNNING},
            OperationState.RUNNING: {
                OperationState.COMPLETED,
                OperationState.FAILED,
                OperationState.CANCELLED,
            },
        }
        with self._condition:
            if state not in allowed.get(record.state, set()):
                raise BrokerError.internal()
            if thread_id is not None:
                record.thread_id = _identifier(thread_id)
            if turn_id is not None:
                record.turn_id = _identifier(turn_id)
            record.result = dict(result) if result is not None else None
            record.error = error
            record.state = state
            record.updated_at = self._clock()
            if _encoded_size(record.to_dict()) > self._max_result_bytes:
                record.thread_id = None
                record.turn_id = None
                record.result = None
                record.error = BrokerError.limit_exceeded()
                record.state = OperationState.FAILED
                if _encoded_size(record.to_dict()) > self._max_result_bytes:
                    raise BrokerError.internal()
            self._condition.notify_all()

    def get(self, entity_id: str, operation_id: str) -> OperationRecord:
        operation_id = _identifier(operation_id)
        with self._condition:
            self._expire()
            entry = self._records.get(operation_id)
            if entry is None or not secrets.compare_digest(entry[0], entity_id):
                raise BrokerError.not_found()
            self._records.move_to_end(operation_id)
            return entry[1]

    def wait(
        self,
        entity_id: str,
        operation_id: str,
        timeout_seconds: float,
        cancelled: threading.Event | None = None,
    ) -> OperationRecord:
        deadline = self._clock() + timeout_seconds
        with self._condition:
            while True:
                if cancelled is not None and cancelled.is_set():
                    raise BrokerError.unavailable()
                record = self.get(entity_id, operation_id)
                if record.state.terminal:
                    return record
                remaining = deadline - self._clock()
                if remaining <= 0:
                    return record
                self._condition.wait(min(remaining, 0.1) if cancelled else remaining)

    def _expire(self) -> None:
        now = self._clock()
        for _, record in self._records.values():
            if (
                record.state is not OperationState.EXPIRED
                and now - record.updated_at >= _OPERATION_TTL_SECONDS
            ):
                record.state = OperationState.EXPIRED
                record.result = None
                record.error = BrokerError.unavailable()
                record.updated_at = now


class SessionOperations:
    """Explicit local APIs for the Stage 2 session lifecycle."""

    def __init__(
        self,
        controller: Any,
        workspaces: WorkspaceRegistry,
        *,
        entity_id: str,
        max_attached_sessions: int,
        max_message_bytes: int,
        max_result_bytes: int,
        max_wait_seconds: int,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        if (
            not isinstance(workspaces, WorkspaceRegistry)
            or not isinstance(entity_id, str)
            or not entity_id
            or len(entity_id) > MAX_ID_LENGTH
        ):
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
        self._controller = controller
        self._workspaces = workspaces
        self._entity_id = entity_id
        self._max_attached_sessions = max_attached_sessions
        self._max_message_bytes = max_message_bytes
        self._max_result_bytes = max_result_bytes
        self._max_wait_seconds = max_wait_seconds
        self._attachments: set[str] = set()
        self._attachment_transitions: set[str] = set()
        self._pending_attachments = 0
        self._active_turns: dict[str, str] = {}
        self._lock = threading.RLock()
        self._store = _OperationStore(
            max_records=min(
                _MAX_OPERATION_RECORDS, max(64, max_attached_sessions * 8)
            ),
            max_result_bytes=max_result_bytes,
            clock=clock,
        )

    def start(self, request: Mapping[str, Any]) -> OperationHandle:
        cwd = self._workspaces.resolve_request(request)
        return self._run(
            "session/start",
            lambda record: self._start(record, cwd),
        )

    def resume(self, request: Mapping[str, Any]) -> OperationHandle:
        thread_id = self._thread_request(request)
        self._allowed_thread(thread_id)
        return self._run(
            "session/resume",
            lambda record: self._resume(record, thread_id),
        )

    def attach(self, request: Mapping[str, Any]) -> OperationHandle:
        thread_id = self._thread_request(request)
        self._allowed_thread(thread_id)
        return self._run(
            "session/attach",
            lambda record: self._attach(record, thread_id),
        )

    def send(self, request: Mapping[str, Any]) -> OperationHandle:
        _exact_fields(request, {"threadId", "message"}, {"threadId", "message"})
        thread_id = _identifier(request["threadId"])
        message = request["message"]
        if not isinstance(message, str) or not message:
            raise BrokerError.invalid_request()
        if len(message.encode("utf-8")) > self._max_message_bytes:
            raise BrokerError.limit_exceeded()
        thread = self._allowed_thread(thread_id)
        if _is_running(thread):
            raise BrokerError.conflict()
        return self._run(
            "session/send",
            lambda record: self._send(record, thread_id, message),
        )

    def status(self, request: Mapping[str, Any]) -> dict[str, Any]:
        thread_id = self._thread_request(request)
        thread = self._allowed_thread(thread_id)
        result: dict[str, Any] = {
            "threadId": thread_id,
            "workspaceId": self._workspace_id(thread),
            "isRunning": _is_running(thread),
            "status": _status_name(thread),
        }
        active_turn_id = _active_turn_id(thread)
        with self._lock:
            if active_turn_id is not None:
                self._active_turns[thread_id] = active_turn_id
            elif not result["isRunning"]:
                self._active_turns.pop(thread_id, None)
            known_turn = self._active_turns.get(thread_id)
        if known_turn is not None and result["isRunning"]:
            result["activeTurnId"] = known_turn
        if _encoded_size(result) > self._max_result_bytes:
            raise BrokerError.limit_exceeded()
        return result

    def wait(self, request: Mapping[str, Any]) -> dict[str, Any]:
        return self._wait_interruptible(request, None)

    def _wait_interruptible(
        self, request: Mapping[str, Any], cancelled: threading.Event | None
    ) -> dict[str, Any]:
        _exact_fields(
            request,
            {"operationId", "timeoutSeconds"},
            {"operationId", "timeoutSeconds"},
        )
        timeout = request["timeoutSeconds"]
        if (
            not isinstance(timeout, (int, float))
            or isinstance(timeout, bool)
            or timeout < 0
            or timeout > self._max_wait_seconds
            or round(float(timeout), _MAX_TIMEOUT_PRECISION) != float(timeout)
        ):
            raise BrokerError.invalid_request()
        return self._store.wait(
            self._entity_id,
            request["operationId"],
            float(timeout),
            cancelled,
        ).to_dict()

    def cancel(self, request: Mapping[str, Any]) -> OperationHandle:
        _exact_fields(request, {"threadId", "turnId"}, {"threadId", "turnId"})
        thread_id = _identifier(request["threadId"])
        turn_id = _identifier(request["turnId"])
        self._allowed_thread(thread_id)
        with self._lock:
            if self._active_turns.get(thread_id) != turn_id:
                raise BrokerError.conflict()
        return self._run(
            "session/cancel",
            lambda record: self._cancel(record, thread_id, turn_id),
        )

    def detach(self, request: Mapping[str, Any]) -> OperationHandle:
        thread_id = self._thread_request(request)
        with self._lock:
            if (
                thread_id not in self._attachments
                or thread_id in self._attachment_transitions
            ):
                raise BrokerError.not_found()
            self._attachment_transitions.add(thread_id)
        try:
            return self._run(
                "session/detach",
                lambda record: self._detach(record, thread_id),
            )
        except BaseException:
            with self._lock:
                self._attachment_transitions.discard(thread_id)
            raise

    def _run(
        self, operation: str, action: Callable[[OperationRecord], None]
    ) -> OperationHandle:
        record = self._store.create(self._entity_id, operation)
        self._store.transition(record, OperationState.RUNNING)
        try:
            action(record)
        except BaseException as error:
            mapped = map_controller_error(error)
            self._store.transition(record, OperationState.FAILED, error=mapped)
        return OperationHandle(record.operation_id)

    def _start(self, record: OperationRecord, cwd: Path) -> None:
        response = self._controller._start_session(str(cwd))
        thread = _thread(response)
        thread_id = _thread_id(thread)
        if not self._workspaces.contains(_cwd(thread)):
            raise BrokerError.unauthorized()
        self._controller._unsubscribe(thread_id)
        self._store.transition(
            record,
            OperationState.COMPLETED,
            thread_id=thread_id,
            result={"threadId": thread_id, "workspaceId": self._workspace_id(thread)},
        )

    def _resume(
        self,
        record: OperationRecord,
        thread_id: str,
    ) -> None:
        with self._lock:
            if (
                thread_id in self._attachments
                or thread_id in self._attachment_transitions
            ):
                raise BrokerError.conflict()
            self._attachment_transitions.add(thread_id)
        try:
            thread = _thread(self._controller._resume_session(thread_id))
            if _thread_id(thread) != thread_id or not self._workspaces.contains(
                _cwd(thread)
            ):
                raise BrokerError.unauthorized()
            self._controller._unsubscribe(thread_id)
        finally:
            with self._lock:
                self._attachment_transitions.discard(thread_id)
        self._store.transition(
            record,
            OperationState.COMPLETED,
            thread_id=thread_id,
            result={"threadId": thread_id, "workspaceId": self._workspace_id(thread)},
        )

    def _attach(self, record: OperationRecord, thread_id: str) -> None:
        with self._lock:
            if thread_id in self._attachment_transitions:
                raise BrokerError.conflict()
            if thread_id in self._attachments:
                self._store.transition(
                    record,
                    OperationState.COMPLETED,
                    thread_id=thread_id,
                    result={"threadId": thread_id},
                )
                return
            if (
                len(self._attachments) + self._pending_attachments
                >= self._max_attached_sessions
            ):
                raise BrokerError.limit_exceeded()
            self._attachment_transitions.add(thread_id)
            self._pending_attachments += 1
        try:
            thread = _thread(self._controller._attach_session(thread_id))
            if _thread_id(thread) != thread_id or not self._workspaces.contains(
                _cwd(thread)
            ):
                raise BrokerError.unauthorized()
            with self._lock:
                self._pending_attachments -= 1
                self._attachments.add(thread_id)
                self._attachment_transitions.remove(thread_id)
        except BaseException:
            with self._lock:
                self._pending_attachments -= 1
                self._attachment_transitions.discard(thread_id)
            raise
        self._store.transition(
            record,
            OperationState.COMPLETED,
            thread_id=thread_id,
            result={"threadId": thread_id, "workspaceId": self._workspace_id(thread)},
        )

    def _send(self, record: OperationRecord, thread_id: str, message: str) -> None:
        response = self._controller._start_turn(thread_id, message)
        turn = response.get("turn") if isinstance(response, Mapping) else None
        if not isinstance(turn, Mapping):
            raise BrokerError.internal()
        turn_id = _identifier(turn.get("id", turn.get("turnId")))
        with self._lock:
            self._active_turns[thread_id] = turn_id
        self._store.transition(
            record,
            OperationState.COMPLETED,
            thread_id=thread_id,
            turn_id=turn_id,
            result={"threadId": thread_id, "turnId": turn_id},
        )

    def _cancel(
        self, record: OperationRecord, thread_id: str, turn_id: str
    ) -> None:
        self._controller._interrupt_turn(thread_id, turn_id)
        with self._lock:
            self._active_turns.pop(thread_id, None)
        self._store.transition(
            record,
            OperationState.CANCELLED,
            thread_id=thread_id,
            turn_id=turn_id,
            result={"threadId": thread_id, "turnId": turn_id},
        )

    def _detach(self, record: OperationRecord, thread_id: str) -> None:
        try:
            self._controller._unsubscribe(thread_id)
            with self._lock:
                self._attachments.remove(thread_id)
                self._active_turns.pop(thread_id, None)
        finally:
            with self._lock:
                self._attachment_transitions.discard(thread_id)
        self._store.transition(
            record,
            OperationState.COMPLETED,
            thread_id=thread_id,
            result={"threadId": thread_id},
        )

    def _allowed_thread(self, thread_id: str) -> Mapping[str, Any]:
        thread = _thread(
            self._controller._read_session(thread_id, include_turns=False)
        )
        if _thread_id(thread) != thread_id:
            raise BrokerError.internal()
        self._workspace_id(thread)
        return thread

    def _thread_request(self, request: Mapping[str, Any]) -> str:
        _exact_fields(request, {"threadId"}, {"threadId"})
        return _identifier(request["threadId"])

    def _workspace_id(self, thread: Mapping[str, Any]) -> str:
        workspace = self._workspaces.workspace_for_path(_cwd(thread))
        if workspace is None:
            raise BrokerError.unauthorized()
        return workspace.workspace_id

def _exact_fields(
    request: Mapping[str, Any], allowed: set[str], required: set[str]
) -> None:
    if (
        not isinstance(request, Mapping)
        or set(request) - allowed
        or not required.issubset(request)
    ):
        raise BrokerError.invalid_request()


def _identifier(value: Any) -> str:
    if not isinstance(value, str) or not value or len(value) > MAX_ID_LENGTH:
        raise BrokerError.invalid_request()
    return value


def _thread(response: Any) -> Mapping[str, Any]:
    if not isinstance(response, Mapping) or not isinstance(
        response.get("thread"), Mapping
    ):
        raise BrokerError.internal()
    return response["thread"]


def _thread_id(thread: Mapping[str, Any]) -> str:
    return _identifier(thread.get("id", thread.get("threadId")))


def _cwd(thread: Mapping[str, Any]) -> str:
    cwd = thread.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        raise BrokerError.unauthorized()
    return cwd


def _status_name(thread: Mapping[str, Any]) -> str:
    status = thread.get("status")
    if isinstance(status, Mapping):
        value = status.get("type")
    else:
        value = status
    return value if isinstance(value, str) and len(value) <= 64 else "unknown"


def _is_running(thread: Mapping[str, Any]) -> bool:
    running = thread.get("isRunning")
    if isinstance(running, bool):
        return running
    return _status_name(thread) in {"active", "inProgress"}


def _active_turn_id(thread: Mapping[str, Any]) -> str | None:
    turns = thread.get("turns")
    if not isinstance(turns, list):
        return None
    for turn in reversed(turns):
        if not isinstance(turn, Mapping):
            continue
        if turn.get("status") in {"inProgress", "active"}:
            return _identifier(turn.get("id", turn.get("turnId")))
    return None


def _encoded_size(value: Any) -> int:
    try:
        encoded = json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
    return len(encoded)
