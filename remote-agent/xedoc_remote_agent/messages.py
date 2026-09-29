"""Persistent, bounded broker delivery for session-to-session messages."""

from __future__ import annotations

from contextlib import contextmanager
from dataclasses import dataclass, replace
import secrets
import sqlite3
import threading
import time
from typing import Any, Callable, Iterator, Mapping

from .errors import BrokerError, ErrorCode, map_controller_error
from .message_contract import (
    DELIVERY_DEFER,
    DELIVERY_INTERRUPT,
    DELIVERY_STEER,
    MessageSource,
    MessageSubmission,
    MessageTarget,
    identifier,
    validate_local_message,
    validate_message_receipt,
    validate_peer_message,
)
from .peer_state import (
    GrantPolicy,
    PeerState,
    _prepare_private_directory,
    _prepare_private_file,
    _secure_sqlite_files,
)
from .workspaces import WorkspaceRegistry


_STATE_NAME = "message-state.sqlite3"
_MESSAGE_TTL_SECONDS = 3_600
_MAX_RECORDS = 8_192
_MAX_INBOX_PER_TARGET = 64
_MAX_CORRELATION_MESSAGES = 16
_DELIVERY_BATCH_SIZE = 16
_POLL_SECONDS = 0.25
_TERMINAL_STATES = {"delivered", "forwarded", "failed", "expired"}
_IN_FLIGHT_STATES = {"forwarding", "delivering", "interrupting"}
_STATES = _TERMINAL_STATES | _IN_FLIGHT_STATES | {"queued"}


@dataclass(frozen=True)
class _StoredMessage:
    source: MessageSource
    submission: MessageSubmission
    state: str
    created_at: int
    expires_at: int


@dataclass(frozen=True)
class _TargetSnapshot:
    is_running: bool
    active_turn_id: str | None
    can_accept_direct_input: bool


class MessageService:
    """Own durable, provenance-preserving message delivery independently of operations."""

    def __init__(
        self,
        directory: str,
        controller: Any,
        workspaces: WorkspaceRegistry,
        state: PeerState,
        *,
        host_id: str,
        max_attached_sessions: int,
        max_message_bytes: int,
        max_result_bytes: int,
        clock: Callable[[], float] = time.time,
    ) -> None:
        required = (
            "_read_session",
            "_start_turn",
            "_steer_turn",
            "_interrupt_turn",
        )
        if (
            not isinstance(workspaces, WorkspaceRegistry)
            or not isinstance(state, PeerState)
            or any(not callable(getattr(controller, name, None)) for name in required)
            or not isinstance(max_attached_sessions, int)
            or isinstance(max_attached_sessions, bool)
            or max_attached_sessions <= 0
            or not isinstance(max_message_bytes, int)
            or isinstance(max_message_bytes, bool)
            or max_message_bytes <= 0
            or not isinstance(max_result_bytes, int)
            or isinstance(max_result_bytes, bool)
            or max_result_bytes < 256
        ):
            raise BrokerError.invalid_request()
        self._directory = state.directory
        if str(self._directory) != directory:
            raise BrokerError.invalid_request()
        self._controller = controller
        self._workspaces = workspaces
        self._state = state
        self._host_id = identifier(host_id)
        self._max_message_bytes = max_message_bytes
        self._max_result_bytes = max_result_bytes
        self._max_records = min(
            _MAX_RECORDS, max(64, max_attached_sessions * 8)
        )
        self._max_inbox_per_target = min(
            _MAX_INBOX_PER_TARGET, max(1, max_attached_sessions)
        )
        self._clock = clock
        self._database_path = self._directory / _STATE_NAME
        self._lock = threading.RLock()
        self._stopping = threading.Event()
        self._wake = threading.Event()
        self._worker: threading.Thread | None = None
        self._peer_service: Any | None = None
        _prepare_private_directory(self._directory)
        _prepare_private_file(self._database_path)
        self._initialize()

    def set_peer_service(self, peer_service: Any) -> None:
        """Attach the fixed paired-broker dispatcher after both services exist."""

        if not callable(getattr(peer_service, "peer_message", None)):
            raise BrokerError.invalid_request()
        with self._lock:
            if self._peer_service is not None:
                raise BrokerError.conflict()
            self._peer_service = peer_service

    def start(self) -> None:
        """Start bounded local inbox delivery."""

        with self._lock:
            worker = self._worker
            if worker is not None and worker.is_alive():
                return
            self._stopping.clear()
            self._wake.set()
            self._worker = threading.Thread(
                target=self._run,
                name="xedoc-remote-agent-messages",
                daemon=True,
            )
            self._worker.start()

    def stop(self) -> None:
        """Stop deferred delivery before the broker controller closes."""

        self._stopping.set()
        self._wake.set()
        with self._lock:
            worker = self._worker
        if worker is not None and worker is not threading.current_thread():
            worker.join(timeout=1.0)
        with self._lock:
            if self._worker is worker and (worker is None or not worker.is_alive()):
                self._worker = None

    def submit_local(
        self, source: MessageSource, params: Mapping[str, Any]
    ) -> dict[str, Any]:
        """Accept one model-originated message using broker-derived provenance."""

        if source.host_id != self._host_id:
            raise BrokerError.unauthorized()
        try:
            submission = validate_local_message(params, self._max_message_bytes)
            return self._submit(source, submission, None, _authorize_local)
        except BrokerError as error:
            self._audit(source, "message.rejected", "denied", error.code.value)
            raise

    def receive_peer(
        self,
        authenticated_host_id: str,
        params: Mapping[str, Any],
        policy: GrantPolicy,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        """Accept a signed peer message only for this host's selected target."""

        authenticated_host_id = identifier(authenticated_host_id)
        if not isinstance(policy, GrantPolicy) or not callable(ensure_authorized):
            raise BrokerError.invalid_request()
        try:
            source, submission = validate_peer_message(
                params, self._max_message_bytes
            )
            if source.host_id != authenticated_host_id:
                raise BrokerError.unauthorized()
            target = replace(submission.target, host_id=self._host_id)
            return self._submit(
                source,
                replace(submission, target=target),
                policy,
                ensure_authorized,
            )
        except BrokerError as error:
            source = _safe_peer_source(params)
            if source is not None:
                self._audit(source, "message.rejected", "denied", error.code.value)
            raise

    def _submit(
        self,
        source: MessageSource,
        submission: MessageSubmission,
        policy: GrantPolicy | None,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        if submission.target.host_id != self._host_id:
            if source.host_id != self._host_id:
                raise BrokerError.unauthorized()
            return self._forward(source, submission)
        return self._accept_target(source, submission, policy, ensure_authorized)

    def _forward(
        self, source: MessageSource, submission: MessageSubmission
    ) -> dict[str, Any]:
        submission = _resolved_submission(submission)
        stored, duplicate = self._insert(source, submission, "forwarding")
        if duplicate:
            return self._receipt(stored)
        self._audit(source, "message.accepted", "ok", None)
        peer_service = self._peer_service
        if peer_service is None:
            self._transition(stored, "failed")
            self._audit(source, "message.rejected", "denied", ErrorCode.UNAVAILABLE.value)
            raise BrokerError.unavailable()
        try:
            response = peer_service.peer_message(source, stored.submission)
            receipt = validate_message_receipt(response, self._max_result_bytes)
            _assert_forwarded_receipt(receipt, stored)
            self._transition(stored, "forwarded")
            self._audit(source, "message.forwarded", "ok", None)
            return self._receipt(replace(stored, state="forwarded"))
        except BaseException as error:
            mapped = map_controller_error(error)
            self._transition(stored, "failed")
            self._audit(source, "message.rejected", "denied", mapped.code.value)
            raise mapped

    def _accept_target(
        self,
        source: MessageSource,
        submission: MessageSubmission,
        policy: GrantPolicy | None,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        submission = _resolved_submission(submission)
        _transcript_message(
            _StoredMessage(source, submission, "queued", 0, 0),
            self._max_message_bytes,
        )
        include_turns = submission.delivery != DELIVERY_DEFER
        snapshot = self._target_snapshot(
            submission.target.thread_id,
            policy,
            ensure_authorized,
            include_turns=include_turns,
        )
        if submission.delivery != DELIVERY_DEFER:
            self._assert_active_delivery(submission, snapshot)
        stored, duplicate = self._insert(source, submission, "queued")
        if duplicate:
            return self._receipt(stored)
        self._audit(source, "message.accepted", "ok", None)
        if submission.delivery == DELIVERY_STEER:
            return self._steer(stored, policy, ensure_authorized)
        if submission.delivery == DELIVERY_INTERRUPT:
            return self._interrupt_then_queue(stored, ensure_authorized)
        self._wake.set()
        return self._receipt(stored)

    def _steer(
        self,
        stored: _StoredMessage,
        policy: GrantPolicy | None,
        ensure_authorized: Callable[[], None],
    ) -> dict[str, Any]:
        target = stored.submission.target
        try:
            snapshot = self._target_snapshot(
                target.thread_id, policy, ensure_authorized, include_turns=True
            )
            self._assert_active_delivery(stored.submission, snapshot)
            ensure_authorized()
            if not self._claim(stored, "delivering"):
                raise BrokerError.conflict()
            response = self._controller._steer_turn(
                target.thread_id,
                target.active_turn_id,
                _transcript_message(stored, self._max_message_bytes),
                stored.submission.message_id,
            )
            if _turn_id(response) != target.active_turn_id:
                raise BrokerError.conflict()
            self._transition(stored, "delivered")
            self._audit(stored.source, "message.delivered", "ok", None)
            return self._receipt(replace(stored, state="delivered"))
        except BaseException as error:
            mapped = map_controller_error(error)
            self._transition(stored, "failed")
            self._audit(stored.source, "message.rejected", "denied", mapped.code.value)
            raise mapped

    def _interrupt_then_queue(
        self, stored: _StoredMessage, ensure_authorized: Callable[[], None]
    ) -> dict[str, Any]:
        target = stored.submission.target
        try:
            ensure_authorized()
            if not self._claim(stored, "interrupting"):
                raise BrokerError.conflict()
            self._controller._interrupt_turn(target.thread_id, target.active_turn_id)
            self._requeue(stored, "interrupting")
        except BaseException as error:
            mapped = map_controller_error(error)
            self._transition(stored, "failed")
            self._audit(stored.source, "message.rejected", "denied", mapped.code.value)
            raise mapped
        self._wake.set()
        return self._receipt(stored)

    def _run(self) -> None:
        while not self._stopping.is_set():
            self._expire()
            for stored in self._queued_messages():
                if self._stopping.is_set():
                    return
                self._deliver_when_idle(stored)
            self._wake.wait(_POLL_SECONDS)
            self._wake.clear()

    def _deliver_when_idle(self, stored: _StoredMessage) -> None:
        claimed = False
        try:
            policy, ensure_authorized = self._delivery_admission(stored)
            snapshot = self._target_snapshot(
                stored.submission.target.thread_id,
                policy,
                ensure_authorized,
                include_turns=False,
            )
            if snapshot.is_running:
                return
            ensure_authorized()
            if not self._claim(stored, "delivering"):
                return
            claimed = True
            _turn_id(
                self._controller._start_turn(
                    stored.submission.target.thread_id,
                    _transcript_message(stored, self._max_message_bytes),
                )
            )
            self._transition(stored, "delivered")
            self._audit(stored.source, "message.delivered", "ok", None)
        except BaseException as error:
            mapped = map_controller_error(error)
            if mapped.code is ErrorCode.CONFLICT:
                if claimed:
                    self._requeue(stored, "delivering")
                return
            if not claimed and mapped.code is ErrorCode.UNAVAILABLE:
                return
            self._transition(stored, "failed")
            self._audit(stored.source, "message.rejected", "denied", mapped.code.value)

    def _delivery_admission(
        self, stored: _StoredMessage
    ) -> tuple[GrantPolicy | None, Callable[[], None]]:
        if stored.source.host_id == self._host_id:
            return None, _authorize_local
        relationship = self._state.relationship(stored.source.host_id)
        if relationship is None:
            raise BrokerError.unauthorized()
        scope = (
            "cancellation"
            if stored.submission.delivery == DELIVERY_INTERRUPT
            else "sessionWrite"
        )
        admission = self._state.admit_write(
            peer_host_id=stored.source.host_id,
            peer_role=relationship.peer_role,
            certificate_fingerprint=relationship.certificate.fingerprint,
            scope=scope,
        )
        return admission.policy, lambda: self._state.validate_admission(admission)

    def _target_snapshot(
        self,
        thread_id: str,
        policy: GrantPolicy | None,
        ensure_authorized: Callable[[], None],
        *,
        include_turns: bool,
    ) -> _TargetSnapshot:
        if policy is not None and not policy.permits_thread(thread_id):
            raise BrokerError.unauthorized()
        ensure_authorized()
        thread = _thread(
            self._controller._read_session(thread_id, include_turns=include_turns)
        )
        if _thread_id(thread) != thread_id:
            raise BrokerError.internal()
        cwd = thread.get("cwd")
        if not isinstance(cwd, str) or not cwd:
            raise BrokerError.unauthorized()
        workspace = self._workspaces.workspace_for_path(cwd)
        if workspace is None:
            raise BrokerError.unauthorized()
        if policy is not None and not policy.permits_session(
            workspace.workspace_id, thread_id
        ):
            raise BrokerError.unauthorized()
        return _TargetSnapshot(
            is_running=_is_running(thread),
            active_turn_id=_active_turn_id(thread),
            can_accept_direct_input=thread.get("canAcceptDirectInput") is True,
        )

    def _assert_active_delivery(
        self, submission: MessageSubmission, snapshot: _TargetSnapshot
    ) -> None:
        if snapshot.active_turn_id != submission.target.active_turn_id:
            raise BrokerError.conflict()
        if (
            submission.delivery == DELIVERY_STEER
            and not snapshot.can_accept_direct_input
        ):
            raise BrokerError.conflict()

    def _insert(
        self, source: MessageSource, submission: MessageSubmission, state: str
    ) -> tuple[_StoredMessage, bool]:
        submission = _resolved_submission(submission)
        self._expire()
        with self._lock, self._transaction() as connection:
            row = connection.execute(
                """
                SELECT source_host_id, source_thread_id, source_extension_id,
                       message_id, correlation_id, target_host_id, target_thread_id,
                       active_turn_id, body, delivery, state, created_at, expires_at
                FROM messages
                WHERE source_host_id = ? AND source_thread_id = ? AND message_id = ?
                """,
                (source.host_id, source.thread_id, submission.message_id),
            ).fetchone()
            if row is not None:
                stored = _stored(row)
                if _same_message(stored, source, submission):
                    return stored, True
                raise BrokerError.conflict()
            self._make_room(connection)
            if submission.delivery in {DELIVERY_DEFER, DELIVERY_INTERRUPT}:
                pending = connection.execute(
                    """
                    SELECT COUNT(*) FROM messages
                    WHERE target_host_id = ? AND target_thread_id = ? AND state = 'queued'
                    """,
                    (submission.target.host_id, submission.target.thread_id),
                ).fetchone()
                if pending is None or int(pending[0]) >= self._max_inbox_per_target:
                    raise BrokerError.limit_exceeded()
            correlation_count = connection.execute(
                "SELECT COUNT(*) FROM messages WHERE correlation_id = ?",
                (submission.correlation_id,),
            ).fetchone()
            if (
                correlation_count is None
                or int(correlation_count[0]) >= _MAX_CORRELATION_MESSAGES
            ):
                raise BrokerError.limit_exceeded()
            now = int(self._clock())
            expires_at = now + _MESSAGE_TTL_SECONDS
            connection.execute(
                """
                INSERT INTO messages (
                    source_host_id, source_thread_id, source_extension_id,
                    message_id, correlation_id, target_host_id, target_thread_id,
                    active_turn_id, body, delivery, state, created_at, expires_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    source.host_id,
                    source.thread_id,
                    source.extension_id,
                    submission.message_id,
                    submission.correlation_id,
                    submission.target.host_id,
                    submission.target.thread_id,
                    submission.target.active_turn_id,
                    submission.body,
                    submission.delivery,
                    state,
                    now,
                    expires_at,
                ),
            )
        return _StoredMessage(source, submission, state, now, expires_at), False

    def _transition(self, stored: _StoredMessage, state: str) -> None:
        if state not in _TERMINAL_STATES:
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            connection.execute(
                """
                UPDATE messages SET state = ?
                WHERE source_host_id = ? AND source_thread_id = ? AND message_id = ?
                  AND state NOT IN ('delivered', 'forwarded', 'failed', 'expired')
                """,
                (
                    state,
                    stored.source.host_id,
                    stored.source.thread_id,
                    stored.submission.message_id,
                ),
            )

    def _claim(self, stored: _StoredMessage, state: str) -> bool:
        if state not in _IN_FLIGHT_STATES:
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            updated = connection.execute(
                """
                UPDATE messages SET state = ?
                WHERE source_host_id = ? AND source_thread_id = ? AND message_id = ?
                  AND state = 'queued'
                """,
                (
                    state,
                    stored.source.host_id,
                    stored.source.thread_id,
                    stored.submission.message_id,
                ),
            ).rowcount
        return updated == 1

    def _requeue(self, stored: _StoredMessage, state: str) -> None:
        if state not in _IN_FLIGHT_STATES:
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            updated = connection.execute(
                """
                UPDATE messages SET state = 'queued'
                WHERE source_host_id = ? AND source_thread_id = ? AND message_id = ?
                  AND state = ?
                """,
                (
                    stored.source.host_id,
                    stored.source.thread_id,
                    stored.submission.message_id,
                    state,
                ),
            ).rowcount
        if updated != 1:
            raise BrokerError.conflict()

    def _queued_messages(self) -> tuple[_StoredMessage, ...]:
        with self._lock, self._connection() as connection:
            rows = connection.execute(
                """
                SELECT source_host_id, source_thread_id, source_extension_id,
                       message_id, correlation_id, target_host_id, target_thread_id,
                       active_turn_id, body, delivery, state, created_at, expires_at
                FROM messages
                WHERE state = 'queued' AND target_host_id = ? AND expires_at > ?
                ORDER BY created_at, message_id
                LIMIT ?
                """,
                (self._host_id, int(self._clock()), _DELIVERY_BATCH_SIZE),
            ).fetchall()
        return tuple(_stored(row) for row in rows)

    def _expire(self) -> None:
        with self._lock, self._transaction() as connection:
            rows = connection.execute(
                """
                SELECT source_host_id, source_thread_id, source_extension_id,
                       message_id, correlation_id, target_host_id, target_thread_id,
                       active_turn_id, body, delivery, state, created_at, expires_at
                FROM messages
                WHERE state IN ('queued', 'forwarding', 'delivering', 'interrupting')
                  AND expires_at <= ?
                """,
                (int(self._clock()),),
            ).fetchall()
            connection.execute(
                """
                UPDATE messages SET state = 'expired'
                WHERE state IN ('queued', 'forwarding', 'delivering', 'interrupting')
                  AND expires_at <= ?
                """,
                (int(self._clock()),),
            )
        for row in rows:
            self._audit(_stored(row).source, "message.expired", "denied", "expired")

    def _make_room(self, connection: sqlite3.Connection) -> None:
        while True:
            count = connection.execute("SELECT COUNT(*) FROM messages").fetchone()
            if count is not None and int(count[0]) < self._max_records:
                return
            removed = connection.execute(
                """
                DELETE FROM messages
                WHERE rowid = (
                    SELECT rowid FROM messages
                    WHERE state IN ('delivered', 'forwarded', 'failed', 'expired')
                    ORDER BY expires_at, created_at
                    LIMIT 1
                )
                """
            ).rowcount
            if removed != 1:
                raise BrokerError.limit_exceeded()

    def _receipt(self, stored: _StoredMessage) -> dict[str, Any]:
        status = {
            "queued": "queued",
            "delivered": "delivered",
            "forwarded": "forwarded",
        }.get(stored.state)
        if status is None:
            raise BrokerError.conflict()
        return validate_message_receipt(
            {
                "messageId": stored.submission.message_id,
                "correlationId": stored.submission.correlation_id,
                "source": {
                    "hostId": stored.source.host_id,
                    "threadId": stored.source.thread_id,
                },
                "target": {
                    "hostId": stored.submission.target.host_id,
                    "threadId": stored.submission.target.thread_id,
                },
                "delivery": stored.submission.delivery,
                "status": status,
            },
            self._max_result_bytes,
        )

    def _audit(
        self, source: MessageSource, action: str, result: str, reason: str | None
    ) -> None:
        self._state.record_audit(
            actor_host_id=source.host_id,
            peer_host_id=(
                None if source.host_id == self._host_id else source.host_id
            ),
            action=action,
            scope="sessionMessage",
            result=result,
            reason=reason,
        )

    def _initialize(self) -> None:
        with self._lock, self._connection() as connection:
            try:
                connection.executescript(
                    """
                    PRAGMA journal_mode = WAL;
                    PRAGMA synchronous = FULL;
                    CREATE TABLE IF NOT EXISTS messages (
                        source_host_id TEXT NOT NULL,
                        source_thread_id TEXT NOT NULL,
                        source_extension_id TEXT NOT NULL,
                        message_id TEXT NOT NULL,
                        correlation_id TEXT NOT NULL,
                        target_host_id TEXT NOT NULL,
                        target_thread_id TEXT NOT NULL,
                        active_turn_id TEXT,
                        body TEXT NOT NULL,
                        delivery TEXT NOT NULL,
                        state TEXT NOT NULL,
                        created_at INTEGER NOT NULL,
                        expires_at INTEGER NOT NULL,
                        PRIMARY KEY (source_host_id, source_thread_id, message_id)
                    );
                    CREATE INDEX IF NOT EXISTS messages_queued_target
                    ON messages (state, target_host_id, target_thread_id, created_at);
                    CREATE INDEX IF NOT EXISTS messages_correlation
                    ON messages (correlation_id);
                    UPDATE messages SET state = 'failed'
                    WHERE state IN ('forwarding', 'delivering', 'interrupting');
                    """
                )
            except sqlite3.Error as error:
                raise BrokerError.unavailable() from error
        _secure_sqlite_files(self._database_path)

    def _connection(self) -> sqlite3.Connection:
        try:
            connection = sqlite3.connect(
                self._database_path,
                timeout=5.0,
                isolation_level=None,
            )
            connection.row_factory = sqlite3.Row
            return connection
        except sqlite3.Error as error:
            raise BrokerError.unavailable() from error

    @contextmanager
    def _transaction(self) -> Iterator[sqlite3.Connection]:
        connection = self._connection()
        try:
            connection.execute("BEGIN IMMEDIATE")
            yield connection
            connection.execute("COMMIT")
        except sqlite3.Error as error:
            try:
                connection.execute("ROLLBACK")
            except sqlite3.Error:
                pass
            raise BrokerError.unavailable() from error
        except BaseException:
            try:
                connection.execute("ROLLBACK")
            except sqlite3.Error:
                pass
            raise
        finally:
            connection.close()
            _secure_sqlite_files(self._database_path)


def _resolved_submission(submission: MessageSubmission) -> MessageSubmission:
    message_id = submission.message_id or f"msg_{_token()}"
    correlation_id = submission.correlation_id or message_id
    return replace(
        submission,
        message_id=identifier(message_id),
        correlation_id=identifier(correlation_id),
    )


def _token() -> str:
    return secrets.token_urlsafe(18)


def _same_message(
    stored: _StoredMessage, source: MessageSource, submission: MessageSubmission
) -> bool:
    return stored.source == source and stored.submission == submission


def _stored(row: sqlite3.Row) -> _StoredMessage:
    try:
        source = MessageSource(
            host_id=identifier(row["source_host_id"]),
            thread_id=identifier(row["source_thread_id"]),
            extension_id=identifier(row["source_extension_id"]),
        )
        active_turn_id = row["active_turn_id"]
        target = MessageTarget(
            host_id=identifier(row["target_host_id"]),
            thread_id=identifier(row["target_thread_id"]),
            active_turn_id=(
                identifier(active_turn_id) if active_turn_id is not None else None
            ),
        )
        message_id = identifier(row["message_id"])
        correlation_id = identifier(row["correlation_id"])
        body = row["body"]
        delivery = row["delivery"]
        state = row["state"]
        created_at = row["created_at"]
        expires_at = row["expires_at"]
        if (
            not isinstance(body, str)
            or not body
            or delivery not in {DELIVERY_DEFER, DELIVERY_STEER, DELIVERY_INTERRUPT}
            or state not in _STATES
            or not isinstance(created_at, int)
            or isinstance(created_at, bool)
            or not isinstance(expires_at, int)
            or isinstance(expires_at, bool)
            or expires_at <= created_at
        ):
            raise BrokerError.internal()
        return _StoredMessage(
            source=source,
            submission=MessageSubmission(
                message_id=message_id,
                correlation_id=correlation_id,
                target=target,
                body=body,
                delivery=delivery,
            ),
            state=state,
            created_at=created_at,
            expires_at=expires_at,
        )
    except (IndexError, KeyError, TypeError, BrokerError) as error:
        raise BrokerError.internal() from error


def _assert_forwarded_receipt(
    receipt: Mapping[str, Any], stored: _StoredMessage
) -> None:
    if (
        receipt.get("messageId") != stored.submission.message_id
        or receipt.get("correlationId") != stored.submission.correlation_id
        or receipt.get("delivery") != stored.submission.delivery
    ):
        raise BrokerError.internal()
    source = receipt.get("source")
    target = receipt.get("target")
    if (
        not isinstance(source, Mapping)
        or source.get("hostId") != stored.source.host_id
        or source.get("threadId") != stored.source.thread_id
        or not isinstance(target, Mapping)
        or target.get("hostId") != stored.submission.target.host_id
        or target.get("threadId") != stored.submission.target.thread_id
    ):
        raise BrokerError.internal()


def _safe_peer_source(value: Mapping[str, Any]) -> MessageSource | None:
    try:
        source = value.get("source")
        if not isinstance(source, Mapping):
            return None
        return MessageSource(
            host_id=identifier(source.get("hostId")),
            thread_id=identifier(source.get("threadId")),
            extension_id=identifier(source.get("extensionId")),
        )
    except BrokerError:
        return None


def _authorize_local() -> None:
    return


def _thread(response: Any) -> Mapping[str, Any]:
    if not isinstance(response, Mapping) or not isinstance(response.get("thread"), Mapping):
        raise BrokerError.internal()
    return response["thread"]


def _thread_id(thread: Mapping[str, Any]) -> str:
    return identifier(thread.get("id", thread.get("threadId")))


def _turn_id(response: Any) -> str:
    if not isinstance(response, Mapping):
        raise BrokerError.internal()
    turn_id = response.get("turnId")
    if turn_id is not None:
        return identifier(turn_id)
    turn = response.get("turn")
    if not isinstance(turn, Mapping):
        raise BrokerError.internal()
    return identifier(turn.get("id", turn.get("turnId")))


def _is_running(thread: Mapping[str, Any]) -> bool:
    running = thread.get("isRunning")
    if isinstance(running, bool):
        return running
    status = thread.get("status")
    if isinstance(status, Mapping):
        status = status.get("type")
    return isinstance(status, str) and status in {"active", "inProgress"}


def _active_turn_id(thread: Mapping[str, Any]) -> str | None:
    turns = thread.get("turns")
    if not isinstance(turns, list):
        return None
    for turn in reversed(turns):
        if not isinstance(turn, Mapping):
            continue
        status = turn.get("status")
        if isinstance(status, Mapping):
            status = status.get("type")
        if isinstance(status, str) and status in {"active", "inProgress"}:
            return identifier(turn.get("id", turn.get("turnId")))
    return None


def _transcript_message(stored: _StoredMessage, max_message_bytes: int) -> str:
    submission = stored.submission
    if submission.message_id is None or submission.correlation_id is None:
        raise BrokerError.internal()
    message = (
        "[Remote agent message]\n"
        f"Origin host: {stored.source.host_id}\n"
        f"Origin thread: {stored.source.thread_id}\n"
        f"Origin extension: {stored.source.extension_id}\n"
        f"Correlation: {submission.correlation_id}\n\n"
        f"{submission.body}"
    )
    if len(message.encode("utf-8")) > max_message_bytes:
        raise BrokerError.limit_exceeded()
    return message
