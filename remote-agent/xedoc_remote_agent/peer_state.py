"""Persistent private identity, relationship, grant, and audit state."""

from __future__ import annotations

from dataclasses import dataclass
import os
from pathlib import Path
import secrets
import sqlite3
import stat
import threading
import time
from typing import Callable, Iterable

from .errors import BrokerError, ErrorCode
from .models import MAX_ID_LENGTH, Role
from .peer_identity import (
    CertificateMaterial,
    HostIdentity,
    certificate_material,
    generated_host_id,
    identity_from_row,
    legacy_certificate_fingerprint,
    legacy_identity_host_id_from_row,
    materialize_identity,
    new_identity,
    validate_host_id,
)


STATE_NAME = "peer-state.sqlite3"
IDENTITY_KEY_NAME = "identity.key"
IDENTITY_CERTIFICATE_NAME = "identity.crt"
MAX_ENDPOINT_BYTES = 2 * 1024
MAX_AUDIT_ROWS = 10_000
MAX_AUDIT_EXPORT_ROWS = 1_000
PROTOCOL_VERSION = 1
STATE_VERSION = "2"
_STATE_VERSION_KEY = "state_version"
_AUDIT_RETENTION_DAYS_KEY = "audit_retention_days"
_LEGACY_STATE_VERSIONS = {None, "1"}
_RELATIONSHIP_STATES = {"pending", "paired", "suspended", "revoked"}
_READ_SCOPES = {"discovery", "workspaceRead", "sessionRead"}
_WRITE_SCOPES = {"sessionWrite", "cancellation"}
_GRANT_SCOPES = _READ_SCOPES | _WRITE_SCOPES


@dataclass(frozen=True)
class Relationship:
    """One local relationship, without local certificate path information."""

    peer_host_id: str
    peer_role: Role
    certificate: CertificateMaterial
    endpoint: str
    status: str
    protocol_version: int
    grant_revision: int
    paired_at: int | None
    updated_at: int

    def public_dict(self) -> dict[str, object]:
        return {
            "hostId": self.peer_host_id,
            "role": self.peer_role.value,
            "fingerprint": self.certificate.fingerprint,
            "endpoint": self.endpoint,
            "status": self.status,
            "protocolVersion": self.protocol_version,
            "grantRevision": self.grant_revision,
        }


@dataclass(frozen=True)
class Grant:
    """A bounded active grant selector for one relationship revision."""

    scope: str
    workspace_id: str | None
    thread_id: str | None
    expires_at: int | None

    def public_dict(self) -> dict[str, object]:
        value: dict[str, object] = {
            "scope": self.scope,
            "expiresAt": self.expires_at,
        }
        if self.workspace_id is not None:
            value["workspaceId"] = self.workspace_id
        if self.thread_id is not None:
            value["threadId"] = self.thread_id
        return value


class GrantPolicy:
    """Evaluates a relationship's current grants without exposing state rows."""

    def __init__(self, grants: tuple[Grant, ...]) -> None:
        self._grants = grants

    def permits_workspace(self, workspace_id: str) -> bool:
        return any(
            grant.workspace_id is None or grant.workspace_id == workspace_id
            for grant in self._grants
        )

    def permits_start(self, workspace_id: str) -> bool:
        return any(
            grant.thread_id is None
            and (grant.workspace_id is None or grant.workspace_id == workspace_id)
            for grant in self._grants
        )

    def permits_thread(self, thread_id: str) -> bool:
        """Reject an explicit thread selector before controller access."""

        return any(
            grant.thread_id is None or grant.thread_id == thread_id
            for grant in self._grants
        )

    def permits_session(self, workspace_id: str, thread_id: str) -> bool:
        return any(
            (grant.workspace_id is None or grant.workspace_id == workspace_id)
            and (grant.thread_id is None or grant.thread_id == thread_id)
            for grant in self._grants
        )


@dataclass(frozen=True)
class ReadAdmission:
    """One revision-bound grant authorization awaiting a bounded response."""

    peer_host_id: str
    peer_role: Role
    certificate_fingerprint: str
    scope: str
    grant_revision: int
    grants: tuple[Grant, ...]

    @property
    def policy(self) -> GrantPolicy:
        return GrantPolicy(self.grants)


class PeerState:
    """SQLite-backed trust state with private permissions and atomic updates."""

    def __init__(
        self,
        directory: str | os.PathLike[str],
        *,
        audit_retention_days: int,
    ) -> None:
        self.directory = Path(directory)
        if not self.directory.is_absolute():
            raise BrokerError.invalid_request()
        if (
            not isinstance(audit_retention_days, int)
            or isinstance(audit_retention_days, bool)
            or audit_retention_days <= 0
        ):
            raise BrokerError.invalid_request()
        self.audit_retention_days = audit_retention_days
        self.database_path = self.directory / STATE_NAME
        self.key_path = self.directory / IDENTITY_KEY_NAME
        self.certificate_path = self.directory / IDENTITY_CERTIFICATE_NAME
        self._lock = threading.RLock()
        _prepare_private_directory(self.directory)
        _prepare_private_file(self.database_path)
        self._initialize()
        self._persist_audit_retention()

    def ensure_identity(self, preferred_host_id: str | None = None) -> HostIdentity:
        """Load or atomically create the durable Ed25519 host identity."""

        if preferred_host_id is not None:
            validate_host_id(preferred_host_id)
        with self._lock, self._transaction() as connection:
            row = connection.execute(
                """
                SELECT host_id, private_key_pem, certificate_pem, fingerprint
                FROM identity WHERE singleton = 1
                """
            ).fetchone()
            if row is None:
                identity = new_identity(preferred_host_id or generated_host_id())
                connection.execute(
                    """
                    INSERT INTO identity (
                        singleton, host_id, private_key_pem, certificate_pem,
                        fingerprint, created_at
                    ) VALUES (1, ?, ?, ?, ?, ?)
                    """,
                    (
                        identity.host_id,
                        identity.private_key_pem,
                        identity.certificate.pem,
                        identity.certificate.fingerprint,
                        _now(),
                    ),
                )
                self._audit(
                    connection,
                    actor_host_id=identity.host_id,
                    peer_host_id=None,
                    action="identity.created",
                    scope=None,
                    result="ok",
                    reason=None,
                )
            else:
                identity = identity_from_row(row)
                if preferred_host_id is not None and identity.host_id != preferred_host_id:
                    raise BrokerError.conflict()
        materialize_identity(self.key_path, self.certificate_path, identity)
        return identity

    def reconcile_role(self, role: Role) -> None:
        """Invalidate grants when the host's configured role changes."""

        if not isinstance(role, Role):
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            row = connection.execute(
                "SELECT value FROM metadata WHERE key = 'role'"
            ).fetchone()
            previous = row[0] if row is not None else None
            if previous is not None and previous != role.value:
                relationships = tuple(
                    _relationship_from_row(row)
                    for row in connection.execute(
                        """
                        SELECT peer_host_id, peer_role, certificate_pem, fingerprint,
                               endpoint, status, protocol_version, grant_revision,
                               paired_at, updated_at
                        FROM relationships WHERE status != 'revoked'
                        """
                    ).fetchall()
                )
                for relationship in relationships:
                    _record_credential_tombstone(connection, relationship)
                connection.execute("DELETE FROM grants")
                connection.execute(
                    """
                    UPDATE relationships
                    SET status = 'revoked', grant_revision = grant_revision + 1,
                        updated_at = ?
                    WHERE status != 'revoked'
                    """,
                    (_now(),),
                )
                self._audit(
                    connection,
                    actor_host_id=None,
                    peer_host_id=None,
                    action="role.changed",
                    scope=None,
                    result="ok",
                    reason=None,
                )
            connection.execute(
                """
                INSERT INTO metadata (key, value) VALUES ('role', ?)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                """,
                (role.value,),
            )
            self._prune_audit(connection)

    def relationship(self, peer_host_id: str) -> Relationship | None:
        validate_host_id(peer_host_id)
        with self._connection() as connection:
            row = connection.execute(
                """
                SELECT peer_host_id, peer_role, certificate_pem, fingerprint,
                       endpoint, status, protocol_version, grant_revision,
                       paired_at, updated_at
                FROM relationships WHERE peer_host_id = ?
                """,
                (peer_host_id,),
            ).fetchone()
        return _relationship_from_row(row) if row is not None else None

    def relationships(self) -> tuple[Relationship, ...]:
        with self._connection() as connection:
            rows = connection.execute(
                """
                SELECT peer_host_id, peer_role, certificate_pem, fingerprint,
                       endpoint, status, protocol_version, grant_revision,
                       paired_at, updated_at
                FROM relationships ORDER BY updated_at DESC, peer_host_id
                LIMIT 128
                """
            ).fetchall()
        return tuple(_relationship_from_row(row) for row in rows)

    def trusted_certificates(self) -> tuple[CertificateMaterial, ...]:
        """Return non-revoked persisted certificates for listener trust roots."""

        with self._connection() as connection:
            rows = connection.execute(
                """
                SELECT certificate_pem, fingerprint FROM relationships
                WHERE status IN ('pending', 'paired', 'suspended')
                ORDER BY peer_host_id
                """
            ).fetchall()
        return tuple(_certificate_from_persisted_row(row) for row in rows)

    def begin_pair(
        self,
        *,
        peer_host_id: str,
        peer_role: Role,
        certificate: CertificateMaterial,
        endpoint: str,
    ) -> Relationship:
        """Persist a locally confirmed pairing intent with no read grants."""

        _validate_relationship_input(peer_host_id, peer_role, certificate, endpoint)
        with self._lock, self._transaction() as connection:
            _reserve_pairing_credential(connection, peer_host_id, certificate)
            existing = _select_relationship(connection, peer_host_id)
            if existing is not None and (
                existing.status == "paired"
                and existing.certificate.fingerprint != certificate.fingerprint
            ):
                raise BrokerError.conflict()
            relationship = self._upsert_relationship(
                connection,
                peer_host_id=peer_host_id,
                peer_role=peer_role,
                certificate=certificate,
                endpoint=endpoint,
                status="pending",
                grant_revision=0,
                paired_at=None,
            )
            connection.execute("DELETE FROM grants WHERE peer_host_id = ?", (peer_host_id,))
            self._audit(
                connection,
                actor_host_id=None,
                peer_host_id=peer_host_id,
                action="pair.pending",
                scope=None,
                result="ok",
                reason=None,
            )
            self._prune_audit(connection)
            return relationship

    def accept_pair(
        self,
        *,
        peer_host_id: str,
        peer_role: Role,
        certificate: CertificateMaterial,
        endpoint: str,
        require_pending: bool,
    ) -> Relationship:
        """Accept a remote pairing only under the caller's verified role policy."""

        _validate_relationship_input(peer_host_id, peer_role, certificate, endpoint)
        with self._lock, self._transaction() as connection:
            _reserve_pairing_credential(connection, peer_host_id, certificate)
            existing = _select_relationship(connection, peer_host_id)
            if require_pending:
                if (
                    existing is None
                    or existing.status != "pending"
                    or existing.peer_role is not peer_role
                    or existing.certificate.fingerprint != certificate.fingerprint
                ):
                    raise BrokerError.conflict()
            elif existing is not None and (
                existing.status == "paired"
                and existing.certificate.fingerprint != certificate.fingerprint
            ):
                raise BrokerError.conflict()
            relationship = self._upsert_relationship(
                connection,
                peer_host_id=peer_host_id,
                peer_role=peer_role,
                certificate=certificate,
                endpoint=endpoint,
                status="paired",
                grant_revision=0,
                paired_at=_now(),
            )
            connection.execute("DELETE FROM grants WHERE peer_host_id = ?", (peer_host_id,))
            self._audit(
                connection,
                actor_host_id=peer_host_id,
                peer_host_id=peer_host_id,
                action="pair.accepted",
                scope=None,
                result="ok",
                reason=None,
            )
            self._prune_audit(connection)
            return relationship

    def complete_pair(
        self,
        *,
        peer_host_id: str,
        certificate: CertificateMaterial,
    ) -> Relationship:
        """Finalize a pairing after its target persisted the relationship."""

        validate_host_id(peer_host_id)
        certificate = _validated_certificate(certificate)
        if certificate.host_id != peer_host_id:
            raise BrokerError.unauthorized()
        with self._lock, self._transaction() as connection:
            existing = _select_relationship(connection, peer_host_id)
            if (
                existing is None
                or existing.status not in {"pending", "paired"}
                or existing.certificate.fingerprint != certificate.fingerprint
            ):
                raise BrokerError.conflict()
            relationship = self._upsert_relationship(
                connection,
                peer_host_id=existing.peer_host_id,
                peer_role=existing.peer_role,
                certificate=existing.certificate,
                endpoint=existing.endpoint,
                status="paired",
                grant_revision=0,
                paired_at=_now(),
            )
            connection.execute("DELETE FROM grants WHERE peer_host_id = ?", (peer_host_id,))
            self._audit(
                connection,
                actor_host_id=None,
                peer_host_id=peer_host_id,
                action="pair.completed",
                scope=None,
                result="ok",
                reason=None,
            )
            self._prune_audit(connection)
            return relationship

    def current_grants(self, peer_host_id: str) -> tuple[Grant, ...]:
        relationship = self.relationship(peer_host_id)
        if relationship is None:
            raise BrokerError.not_found()
        with self._connection() as connection:
            rows = connection.execute(
                """
                SELECT scope, workspace_id, thread_id, expires_at
                FROM grants
                WHERE peer_host_id = ? AND revision = ?
                ORDER BY scope, selector_key
                """,
                (peer_host_id, relationship.grant_revision),
            ).fetchall()
        return tuple(
            Grant(
                scope=str(row[0]),
                workspace_id=str(row[1]) if row[1] is not None else None,
                thread_id=str(row[2]) if row[2] is not None else None,
                expires_at=int(row[3]) if row[3] is not None else None,
            )
            for row in rows
        )

    def next_grant_revision(self, peer_host_id: str) -> int:
        relationship = self.relationship(peer_host_id)
        if relationship is None or relationship.status != "paired":
            raise BrokerError.conflict()
        return relationship.grant_revision + 1

    def replace_grants(
        self,
        *,
        peer_host_id: str,
        revision: int,
        grants: Iterable[Grant],
        actor_host_id: str | None,
    ) -> tuple[Grant, ...]:
        """Atomically replace the sole active grant revision for a relationship."""

        validate_host_id(peer_host_id)
        if actor_host_id is not None:
            validate_host_id(actor_host_id)
        grants = tuple(grants)
        _validate_grants(grants)
        if not isinstance(revision, int) or isinstance(revision, bool) or revision <= 0:
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            relationship = _select_relationship(connection, peer_host_id)
            if relationship is None or relationship.status != "paired":
                raise BrokerError.conflict()
            if revision != relationship.grant_revision + 1:
                raise BrokerError.conflict()
            connection.execute(
                "DELETE FROM grants WHERE peer_host_id = ? AND revision = ?",
                (peer_host_id, revision),
            )
            for grant in grants:
                connection.execute(
                    """
                    INSERT INTO grants (
                        peer_host_id, revision, scope, selector_key,
                        workspace_id, thread_id, expires_at, created_at
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        peer_host_id,
                        revision,
                        grant.scope,
                        _selector_key(grant.workspace_id, grant.thread_id),
                        grant.workspace_id,
                        grant.thread_id,
                        grant.expires_at,
                        _now(),
                    ),
                )
            connection.execute(
                """
                UPDATE relationships
                SET grant_revision = ?, updated_at = ?
                WHERE peer_host_id = ?
                """,
                (revision, _now(), peer_host_id),
            )
            self._audit(
                connection,
                actor_host_id=actor_host_id,
                peer_host_id=peer_host_id,
                action="grant.replaced",
                scope=",".join(grant.scope for grant in grants),
                result="ok",
                reason=None,
            )
            self._prune_audit(connection)
        return grants

    def admit_read(
        self,
        *,
        peer_host_id: str,
        peer_role: Role,
        certificate_fingerprint: str,
        scope: str,
    ) -> ReadAdmission:
        """Create a revision-bound authorization before bounded catalog work."""

        validate_host_id(peer_host_id)
        if peer_role is not Role.COORDINATOR or scope not in _READ_SCOPES:
            raise BrokerError.unauthorized()
        with self._lock, self._transaction() as connection:
            relationship, grants = _active_grants(
                connection,
                peer_host_id=peer_host_id,
                peer_role=peer_role,
                certificate_fingerprint=certificate_fingerprint,
                scope=scope,
            )
            return ReadAdmission(
                peer_host_id=peer_host_id,
                peer_role=peer_role,
                certificate_fingerprint=certificate_fingerprint,
                scope=scope,
                grant_revision=relationship.grant_revision,
                grants=grants,
            )

    def admit_write(
        self,
        *,
        peer_host_id: str,
        peer_role: Role,
        certificate_fingerprint: str,
        scope: str,
    ) -> ReadAdmission:
        """Create a revision-bound authorization before peer session control."""

        validate_host_id(peer_host_id)
        if peer_role is not Role.COORDINATOR or scope not in _WRITE_SCOPES:
            raise BrokerError.unauthorized()
        with self._lock, self._transaction() as connection:
            relationship, grants = _active_grants(
                connection,
                peer_host_id=peer_host_id,
                peer_role=peer_role,
                certificate_fingerprint=certificate_fingerprint,
                scope=scope,
            )
            return ReadAdmission(
                peer_host_id=peer_host_id,
                peer_role=peer_role,
                certificate_fingerprint=certificate_fingerprint,
                scope=scope,
                grant_revision=relationship.grant_revision,
                grants=grants,
            )

    def finalize_read_admission(
        self,
        admission: ReadAdmission,
        *,
        action: str,
        response_factory: Callable[[], dict[str, object]],
    ) -> dict[str, object]:
        """Revalidate and sign one response without locking catalog work."""

        return self.finalize_admission(
            admission,
            action=action,
            response_factory=response_factory,
        )

    def finalize_admission(
        self,
        admission: ReadAdmission,
        *,
        action: str,
        response_factory: Callable[[], dict[str, object]],
    ) -> dict[str, object]:
        """Revalidate a read or write admission before returning its result."""

        if not isinstance(admission, ReadAdmission):
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            relationship, grants = _active_grants(
                connection,
                peer_host_id=admission.peer_host_id,
                peer_role=admission.peer_role,
                certificate_fingerprint=admission.certificate_fingerprint,
                scope=admission.scope,
            )
            if (
                relationship.grant_revision != admission.grant_revision
                or grants != admission.grants
            ):
                raise BrokerError.unauthorized()
            self._audit(
                connection,
                actor_host_id=admission.peer_host_id,
                peer_host_id=admission.peer_host_id,
                action=action,
                scope=admission.scope,
                result="ok",
                reason=None,
            )
            self._prune_audit(connection)
            return response_factory()

    def validate_admission(self, admission: ReadAdmission) -> None:
        """Reject a stale, revoked, expired, or selector-replaced admission."""

        if not isinstance(admission, ReadAdmission):
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            relationship, grants = _active_grants(
                connection,
                peer_host_id=admission.peer_host_id,
                peer_role=admission.peer_role,
                certificate_fingerprint=admission.certificate_fingerprint,
                scope=admission.scope,
            )
            if (
                relationship.grant_revision != admission.grant_revision
                or grants != admission.grants
            ):
                raise BrokerError.unauthorized()

    def authorize_read(
        self,
        *,
        peer_host_id: str,
        peer_role: Role,
        certificate_fingerprint: str,
        scope: str,
    ) -> GrantPolicy:
        """Compatibility wrapper for callers that do not return peer frames."""

        return self.admit_read(
            peer_host_id=peer_host_id,
            peer_role=peer_role,
            certificate_fingerprint=certificate_fingerprint,
            scope=scope,
        ).policy

    def is_credential_tombstoned(self, certificate: CertificateMaterial) -> bool:
        """Return whether a locally revoked credential must stay unusable."""

        certificate = _validated_certificate(certificate)
        with self._connection() as connection:
            row = connection.execute(
                """
                SELECT peer_host_id FROM credential_tombstones
                WHERE fingerprint = ?
                """,
                (certificate.fingerprint,),
            ).fetchone()
        if row is None:
            return False
        if not isinstance(row[0], str) or row[0] != certificate.host_id:
            raise BrokerError.internal()
        return True

    def record_nonce(
        self, *, peer_host_id: str, nonce: str, expires_at: int
    ) -> None:
        """Reject replayed signed request nonces before dispatch."""

        validate_host_id(peer_host_id)
        if (
            not isinstance(nonce, str)
            or not 16 <= len(nonce) <= 256
            or not isinstance(expires_at, int)
            or isinstance(expires_at, bool)
            or expires_at <= _now()
        ):
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            connection.execute(
                "DELETE FROM request_nonces WHERE expires_at <= ?", (_now(),)
            )
            try:
                connection.execute(
                    """
                    INSERT INTO request_nonces (peer_host_id, nonce, expires_at)
                    VALUES (?, ?, ?)
                    """,
                    (peer_host_id, nonce, expires_at),
                )
            except sqlite3.IntegrityError as error:
                raise BrokerError.conflict() from error

    def record_audit(
        self,
        *,
        actor_host_id: str | None,
        peer_host_id: str | None,
        action: str,
        scope: str | None,
        result: str,
        reason: str | None,
    ) -> None:
        """Persist a redacted, bounded audit event."""

        with self._lock, self._transaction() as connection:
            self._audit(
                connection,
                actor_host_id=actor_host_id,
                peer_host_id=peer_host_id,
                action=action,
                scope=scope,
                result=result,
                reason=reason,
            )
            self._prune_audit(connection)

    def export_audit(self, *, limit: int) -> tuple[dict[str, object], ...]:
        """Return redacted audit rows within the active retention window."""

        _validate_audit_export_limit(limit)
        with self._lock, self._transaction() as connection:
            self._prune_audit(connection)
            rows = connection.execute(
                """
                SELECT occurred_at, actor_host_id, peer_host_id, action, scope, result, reason
                FROM audit
                ORDER BY id DESC
                LIMIT ?
                """,
                (limit,),
            ).fetchall()
        return tuple(_audit_export_row(row) for row in reversed(rows))

    def set_status(
        self,
        *,
        peer_host_id: str,
        status: str,
        actor_host_id: str | None,
    ) -> Relationship:
        """Suspend or revoke a relationship before any remote propagation."""

        validate_host_id(peer_host_id)
        if status not in {"suspended", "revoked"}:
            raise BrokerError.invalid_request()
        with self._lock, self._transaction() as connection:
            relationship = _select_relationship(connection, peer_host_id)
            if relationship is None:
                raise BrokerError.not_found()
            revision = relationship.grant_revision + 1
            if status == "revoked":
                connection.execute("DELETE FROM grants WHERE peer_host_id = ?", (peer_host_id,))
                _record_credential_tombstone(connection, relationship)
            updated = self._upsert_relationship(
                connection,
                peer_host_id=relationship.peer_host_id,
                peer_role=relationship.peer_role,
                certificate=relationship.certificate,
                endpoint=relationship.endpoint,
                status=status,
                grant_revision=revision,
                paired_at=relationship.paired_at,
            )
            self._audit(
                connection,
                actor_host_id=actor_host_id,
                peer_host_id=peer_host_id,
                action=f"relationship.{status}",
                scope=None,
                result="ok",
                reason=None,
            )
            self._prune_audit(connection)
            return updated

    def remove(self, *, peer_host_id: str, actor_host_id: str | None) -> None:
        """Remove a relationship and all current grant rows atomically."""

        validate_host_id(peer_host_id)
        with self._lock, self._transaction() as connection:
            relationship = _select_relationship(connection, peer_host_id)
            if relationship is None:
                raise BrokerError.not_found()
            _record_credential_tombstone(connection, relationship)
            connection.execute(
                "DELETE FROM grants WHERE peer_host_id = ?", (peer_host_id,)
            )
            connection.execute(
                "DELETE FROM relationships WHERE peer_host_id = ?", (peer_host_id,)
            )
            self._audit(
                connection,
                actor_host_id=actor_host_id,
                peer_host_id=peer_host_id,
                action="relationship.removed",
                scope=None,
                result="ok",
                reason=None,
            )
            self._prune_audit(connection)

    def _initialize(self) -> None:
        with self._lock:
            connection = self._connection()
            try:
                connection.executescript(
                    """
                    PRAGMA journal_mode = WAL;
                    PRAGMA synchronous = FULL;
                    CREATE TABLE IF NOT EXISTS identity (
                        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                        host_id TEXT NOT NULL,
                        private_key_pem BLOB NOT NULL,
                        certificate_pem BLOB NOT NULL,
                        fingerprint TEXT NOT NULL,
                        created_at INTEGER NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS metadata (
                        key TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS relationships (
                        peer_host_id TEXT PRIMARY KEY,
                        peer_role TEXT NOT NULL,
                        certificate_pem BLOB NOT NULL,
                        fingerprint TEXT NOT NULL,
                        endpoint TEXT NOT NULL,
                        status TEXT NOT NULL,
                        protocol_version INTEGER NOT NULL,
                        grant_revision INTEGER NOT NULL,
                        paired_at INTEGER,
                        updated_at INTEGER NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS grants (
                        peer_host_id TEXT NOT NULL,
                        revision INTEGER NOT NULL,
                        scope TEXT NOT NULL,
                        selector_key TEXT NOT NULL,
                        workspace_id TEXT,
                        thread_id TEXT,
                        expires_at INTEGER,
                        created_at INTEGER NOT NULL,
                        PRIMARY KEY (peer_host_id, revision, scope, selector_key)
                    );
                    CREATE TABLE IF NOT EXISTS request_nonces (
                        peer_host_id TEXT NOT NULL,
                        nonce TEXT NOT NULL,
                        expires_at INTEGER NOT NULL,
                        PRIMARY KEY (peer_host_id, nonce)
                    );
                    CREATE TABLE IF NOT EXISTS peer_credentials (
                        fingerprint TEXT PRIMARY KEY,
                        peer_host_id TEXT NOT NULL,
                        certificate_pem BLOB NOT NULL,
                        first_seen_at INTEGER NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS credential_tombstones (
                        fingerprint TEXT PRIMARY KEY,
                        peer_host_id TEXT NOT NULL,
                        revoked_at INTEGER NOT NULL
                    );
                    CREATE TABLE IF NOT EXISTS audit (
                        id INTEGER PRIMARY KEY AUTOINCREMENT,
                        occurred_at INTEGER NOT NULL,
                        actor_host_id TEXT,
                        peer_host_id TEXT,
                        action TEXT NOT NULL,
                        scope TEXT,
                        result TEXT NOT NULL,
                        reason TEXT
                    );
                    CREATE INDEX IF NOT EXISTS audit_occurred_at ON audit (occurred_at);
                    """
                )
            except sqlite3.Error as error:
                raise BrokerError.unavailable() from error
            finally:
                connection.close()
            with self._transaction() as connection:
                self._migrate_state(connection)
                self._backfill_credential_bindings(connection)
        _secure_sqlite_files(self.database_path)

    def _persist_audit_retention(self) -> None:
        with self._lock, self._transaction() as connection:
            connection.execute(
                """
                INSERT INTO metadata (key, value) VALUES (?, ?)
                ON CONFLICT(key) DO UPDATE SET value = excluded.value
                """,
                (_AUDIT_RETENTION_DAYS_KEY, str(self.audit_retention_days)),
            )
            self._prune_audit(connection)

    def _migrate_state(self, connection: sqlite3.Connection) -> None:
        state_version = _state_version(connection)
        if state_version not in _LEGACY_STATE_VERSIONS | {STATE_VERSION}:
            raise _state_reset_error()
        identity_row = connection.execute(
            """
            SELECT host_id, private_key_pem, certificate_pem, fingerprint
            FROM identity WHERE singleton = 1
            """
        ).fetchone()
        if identity_row is None:
            if _has_peer_security_state(connection):
                raise _state_reset_error()
            _set_state_version(connection)
            return
        try:
            identity_from_row(identity_row)
        except BrokerError as error:
            if error.code is ErrorCode.UNAVAILABLE:
                raise
            if state_version == STATE_VERSION:
                raise _state_reset_error() from error
            self._migrate_legacy_identity(connection, identity_row)
            return
        try:
            _validate_current_relationship_rows(connection)
        except BrokerError as error:
            if error.code is ErrorCode.UNAVAILABLE:
                raise
            raise _state_reset_error() from error
        _set_state_version(connection)

    def _migrate_legacy_identity(
        self,
        connection: sqlite3.Connection,
        identity_row: tuple[object, ...],
    ) -> None:
        try:
            host_id = legacy_identity_host_id_from_row(identity_row)
            _validate_legacy_relationship_rows(connection)
            _validate_legacy_credential_rows(connection)
        except BrokerError as error:
            if error.code is ErrorCode.UNAVAILABLE:
                raise
            raise _state_reset_error() from error
        replacement = new_identity(host_id)
        connection.execute(
            """
            UPDATE identity
            SET private_key_pem = ?, certificate_pem = ?, fingerprint = ?, created_at = ?
            WHERE singleton = 1
            """,
            (
                replacement.private_key_pem,
                replacement.certificate.pem,
                replacement.certificate.fingerprint,
                _now(),
            ),
        )
        for table in (
            "grants",
            "relationships",
            "peer_credentials",
            "credential_tombstones",
            "request_nonces",
        ):
            connection.execute(f"DELETE FROM {table}")
        self._audit(
            connection,
            actor_host_id=host_id,
            peer_host_id=None,
            action="state.migrated",
            scope=None,
            result="ok",
            reason="legacySan",
        )
        _set_state_version(connection)
        self._prune_audit(connection)

    def _backfill_credential_bindings(self, connection: sqlite3.Connection) -> None:
        rows = connection.execute(
            """
            SELECT peer_host_id, peer_role, certificate_pem, fingerprint,
                   endpoint, status, protocol_version, grant_revision,
                   paired_at, updated_at
            FROM relationships
            """
        ).fetchall()
        for row in rows:
            relationship = _relationship_from_row(row)
            _record_credential_binding(
                connection,
                peer_host_id=relationship.peer_host_id,
                certificate=relationship.certificate,
            )
            if relationship.status == "revoked":
                _record_credential_tombstone(connection, relationship)

    def _connection(self) -> sqlite3.Connection:
        try:
            connection = sqlite3.connect(
                self.database_path,
                timeout=5.0,
                isolation_level=None,
            )
        except sqlite3.Error as error:
            raise BrokerError.unavailable() from error
        try:
            connection.execute("PRAGMA foreign_keys = ON")
        except sqlite3.Error as error:
            connection.close()
            raise BrokerError.unavailable() from error
        return connection

    def _transaction(self):
        return _StateTransaction(self)

    def _upsert_relationship(
        self,
        connection: sqlite3.Connection,
        *,
        peer_host_id: str,
        peer_role: Role,
        certificate: CertificateMaterial,
        endpoint: str,
        status: str,
        grant_revision: int,
        paired_at: int | None,
    ) -> Relationship:
        if status not in _RELATIONSHIP_STATES:
            raise BrokerError.invalid_request()
        updated_at = _now()
        connection.execute(
            """
            INSERT INTO relationships (
                peer_host_id, peer_role, certificate_pem, fingerprint, endpoint,
                status, protocol_version, grant_revision, paired_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(peer_host_id) DO UPDATE SET
                peer_role = excluded.peer_role,
                certificate_pem = excluded.certificate_pem,
                fingerprint = excluded.fingerprint,
                endpoint = excluded.endpoint,
                status = excluded.status,
                protocol_version = excluded.protocol_version,
                grant_revision = excluded.grant_revision,
                paired_at = excluded.paired_at,
                updated_at = excluded.updated_at
            """,
            (
                peer_host_id,
                peer_role.value,
                certificate.pem,
                certificate.fingerprint,
                endpoint,
                status,
                PROTOCOL_VERSION,
                grant_revision,
                paired_at,
                updated_at,
            ),
        )
        return Relationship(
            peer_host_id=peer_host_id,
            peer_role=peer_role,
            certificate=certificate,
            endpoint=endpoint,
            status=status,
            protocol_version=PROTOCOL_VERSION,
            grant_revision=grant_revision,
            paired_at=paired_at,
            updated_at=updated_at,
        )

    def _audit(
        self,
        connection: sqlite3.Connection,
        *,
        actor_host_id: str | None,
        peer_host_id: str | None,
        action: str,
        scope: str | None,
        result: str,
        reason: str | None,
    ) -> None:
        for value, limit in ((action, 128), (result, 32)):
            if not _is_redacted_audit_text(value, limit):
                raise BrokerError.invalid_request()
        if scope is not None and not _is_redacted_audit_text(scope, 256):
            raise BrokerError.invalid_request()
        if reason is not None and not _is_redacted_audit_text(reason, 128):
            raise BrokerError.invalid_request()
        if (
            actor_host_id is not None
            and not _is_redacted_audit_text(actor_host_id, 128)
        ) or (
            peer_host_id is not None
            and not _is_redacted_audit_text(peer_host_id, 128)
        ):
            raise BrokerError.invalid_request()
        connection.execute(
            """
            INSERT INTO audit (
                occurred_at, actor_host_id, peer_host_id, action, scope, result, reason
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            (
                _now(),
                actor_host_id,
                peer_host_id,
                action,
                scope,
                result,
                reason,
            ),
        )

    def _prune_audit(self, connection: sqlite3.Connection) -> None:
        connection.execute(
            "DELETE FROM audit WHERE occurred_at < ?",
            (_now() - self.audit_retention_days * 86_400,),
        )
        connection.execute(
            """
            DELETE FROM audit
            WHERE id NOT IN (
                SELECT id FROM audit ORDER BY id DESC LIMIT ?
            )
            """,
            (MAX_AUDIT_ROWS,),
        )


class _StateTransaction:
    def __init__(self, state: PeerState) -> None:
        self._state = state
        self.connection: sqlite3.Connection | None = None

    def __enter__(self) -> sqlite3.Connection:
        self.connection = self._state._connection()
        try:
            self.connection.execute("BEGIN IMMEDIATE")
        except sqlite3.Error as error:
            self.connection.close()
            raise BrokerError.unavailable() from error
        return self.connection

    def __exit__(self, exception_type: object, *_: object) -> None:
        if self.connection is None:
            return
        try:
            if exception_type is None:
                self.connection.execute("COMMIT")
            else:
                self.connection.execute("ROLLBACK")
        except sqlite3.Error as error:
            raise BrokerError.unavailable() from error
        finally:
            self.connection.close()
            _secure_sqlite_files(self._state.database_path)


def _state_version(connection: sqlite3.Connection) -> str | None:
    row = connection.execute(
        "SELECT value FROM metadata WHERE key = ?",
        (_STATE_VERSION_KEY,),
    ).fetchone()
    if row is None:
        return None
    if len(row) != 1 or not isinstance(row[0], str):
        raise _state_reset_error()
    return row[0]


def _set_state_version(connection: sqlite3.Connection) -> None:
    connection.execute(
        """
        INSERT INTO metadata (key, value) VALUES (?, ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        """,
        (_STATE_VERSION_KEY, STATE_VERSION),
    )


def _has_peer_security_state(connection: sqlite3.Connection) -> bool:
    row = connection.execute(
        """
        SELECT
            EXISTS(SELECT 1 FROM relationships)
            OR EXISTS(SELECT 1 FROM grants)
            OR EXISTS(SELECT 1 FROM request_nonces)
            OR EXISTS(SELECT 1 FROM peer_credentials)
            OR EXISTS(SELECT 1 FROM credential_tombstones)
        """
    ).fetchone()
    if len(row) != 1 or row[0] not in {0, 1}:
        raise _state_reset_error()
    return bool(row[0])


def _state_reset_error() -> BrokerError:
    return BrokerError.conflict("remote-agent peer state requires reset")


def _validate_current_relationship_rows(connection: sqlite3.Connection) -> None:
    rows = connection.execute(
        """
        SELECT peer_host_id, peer_role, certificate_pem, fingerprint,
               endpoint, status, protocol_version, grant_revision,
               paired_at, updated_at
        FROM relationships
        """
    ).fetchall()
    for row in rows:
        _relationship_from_row(row)


def _validate_legacy_relationship_rows(connection: sqlite3.Connection) -> None:
    rows = connection.execute(
        """
        SELECT peer_host_id, peer_role, certificate_pem, fingerprint,
               endpoint, status, protocol_version, grant_revision,
               paired_at, updated_at
        FROM relationships
        """
    ).fetchall()
    for row in rows:
        _validate_legacy_relationship_row(row)


def _validate_legacy_relationship_row(row: tuple[object, ...]) -> None:
    if len(row) != 10:
        raise BrokerError.internal()
    (
        peer_host_id,
        peer_role,
        certificate_pem,
        fingerprint,
        endpoint,
        status,
        protocol_version,
        grant_revision,
        paired_at,
        updated_at,
    ) = row
    if (
        not isinstance(peer_host_id, str)
        or not isinstance(peer_role, str)
        or not isinstance(certificate_pem, bytes)
        or not _is_fingerprint(fingerprint)
        or not isinstance(endpoint, str)
        or not isinstance(status, str)
        or not isinstance(protocol_version, int)
        or isinstance(protocol_version, bool)
        or not isinstance(grant_revision, int)
        or isinstance(grant_revision, bool)
        or not isinstance(updated_at, int)
        or isinstance(updated_at, bool)
        or status not in _RELATIONSHIP_STATES
        or protocol_version != PROTOCOL_VERSION
        or grant_revision < 0
        or (
            paired_at is not None
            and (not isinstance(paired_at, int) or isinstance(paired_at, bool))
        )
    ):
        raise BrokerError.internal()
    validate_host_id(peer_host_id)
    try:
        Role(peer_role)
        endpoint_bytes = len(endpoint.encode("utf-8"))
    except (UnicodeError, ValueError) as error:
        raise BrokerError.internal() from error
    if not 0 < endpoint_bytes <= MAX_ENDPOINT_BYTES:
        raise BrokerError.internal()
    if not secrets.compare_digest(
        legacy_certificate_fingerprint(certificate_pem),
        fingerprint,
    ):
        raise BrokerError.internal()


def _validate_legacy_credential_rows(connection: sqlite3.Connection) -> None:
    credential_rows = connection.execute(
        """
        SELECT fingerprint, peer_host_id, certificate_pem, first_seen_at
        FROM peer_credentials
        """
    ).fetchall()
    for row in credential_rows:
        if (
            len(row) != 4
            or not _is_fingerprint(row[0])
            or not isinstance(row[1], str)
            or not isinstance(row[2], bytes)
            or not isinstance(row[3], int)
            or isinstance(row[3], bool)
            or row[3] < 0
        ):
            raise BrokerError.internal()
        validate_host_id(row[1])
        if not secrets.compare_digest(legacy_certificate_fingerprint(row[2]), row[0]):
            raise BrokerError.internal()
    tombstone_rows = connection.execute(
        """
        SELECT fingerprint, peer_host_id, revoked_at
        FROM credential_tombstones
        """
    ).fetchall()
    for row in tombstone_rows:
        if (
            len(row) != 3
            or not _is_fingerprint(row[0])
            or not isinstance(row[1], str)
            or not isinstance(row[2], int)
            or isinstance(row[2], bool)
            or row[2] < 0
        ):
            raise BrokerError.internal()
        validate_host_id(row[1])


def _is_fingerprint(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def _relationship_from_row(row: tuple[object, ...]) -> Relationship:
    if len(row) != 10:
        raise BrokerError.internal()
    (
        peer_host_id,
        peer_role,
        certificate_pem,
        fingerprint,
        endpoint,
        status,
        protocol_version,
        grant_revision,
        paired_at,
        updated_at,
    ) = row
    if (
        not isinstance(peer_host_id, str)
        or not isinstance(peer_role, str)
        or not isinstance(certificate_pem, bytes)
        or not isinstance(fingerprint, str)
        or not isinstance(endpoint, str)
        or not isinstance(status, str)
        or not isinstance(protocol_version, int)
        or not isinstance(grant_revision, int)
        or not isinstance(updated_at, int)
        or status not in _RELATIONSHIP_STATES
        or protocol_version != PROTOCOL_VERSION
        or grant_revision < 0
    ):
        raise BrokerError.internal()
    validate_host_id(peer_host_id)
    try:
        role = Role(peer_role)
    except ValueError as error:
        raise BrokerError.internal() from error
    try:
        material = certificate_material(certificate_pem)
    except BrokerError as error:
        raise BrokerError.internal() from error
    if not secrets.compare_digest(material.fingerprint, fingerprint):
        raise BrokerError.internal()
    if material.host_id != peer_host_id:
        raise BrokerError.internal()
    if paired_at is not None and not isinstance(paired_at, int):
        raise BrokerError.internal()
    return Relationship(
        peer_host_id=peer_host_id,
        peer_role=role,
        certificate=material,
        endpoint=endpoint,
        status=status,
        protocol_version=protocol_version,
        grant_revision=grant_revision,
        paired_at=paired_at,
        updated_at=updated_at,
    )


def _select_relationship(
    connection: sqlite3.Connection, peer_host_id: str
) -> Relationship | None:
    row = connection.execute(
        """
        SELECT peer_host_id, peer_role, certificate_pem, fingerprint,
               endpoint, status, protocol_version, grant_revision,
               paired_at, updated_at
        FROM relationships WHERE peer_host_id = ?
        """,
        (peer_host_id,),
    ).fetchone()
    return _relationship_from_row(row) if row is not None else None


def _validate_relationship_input(
    peer_host_id: str,
    peer_role: Role,
    certificate: CertificateMaterial,
    endpoint: str,
) -> None:
    validate_host_id(peer_host_id)
    if not isinstance(peer_role, Role) or not isinstance(certificate, CertificateMaterial):
        raise BrokerError.invalid_request()
    certificate = _validated_certificate(certificate)
    if certificate.host_id != peer_host_id:
        raise BrokerError.invalid_request()
    if (
        not isinstance(endpoint, str)
        or not endpoint
        or len(endpoint.encode("utf-8")) > MAX_ENDPOINT_BYTES
    ):
        raise BrokerError.invalid_request()


def _validated_certificate(certificate: CertificateMaterial) -> CertificateMaterial:
    if not isinstance(certificate, CertificateMaterial):
        raise BrokerError.invalid_request()
    material = certificate_material(certificate.pem)
    if (
        not secrets.compare_digest(material.fingerprint, certificate.fingerprint)
        or material.host_id != certificate.host_id
    ):
        raise BrokerError.invalid_request()
    return material


def _certificate_from_persisted_row(row: tuple[object, ...]) -> CertificateMaterial:
    if (
        len(row) != 2
        or not isinstance(row[0], bytes)
        or not isinstance(row[1], str)
    ):
        raise BrokerError.internal()
    try:
        material = certificate_material(row[0])
    except BrokerError as error:
        raise BrokerError.internal() from error
    if not secrets.compare_digest(material.fingerprint, row[1]):
        raise BrokerError.internal()
    return material


def _reserve_pairing_credential(
    connection: sqlite3.Connection,
    peer_host_id: str,
    certificate: CertificateMaterial,
) -> None:
    _record_credential_binding(
        connection,
        peer_host_id=peer_host_id,
        certificate=certificate,
    )
    row = connection.execute(
        """
        SELECT peer_host_id FROM credential_tombstones
        WHERE fingerprint = ?
        """,
        (certificate.fingerprint,),
    ).fetchone()
    if row is None:
        return
    if not isinstance(row[0], str) or row[0] != peer_host_id:
        raise BrokerError.internal()
    raise BrokerError.unauthorized()


def _record_credential_binding(
    connection: sqlite3.Connection,
    *,
    peer_host_id: str,
    certificate: CertificateMaterial,
) -> None:
    certificate = _validated_certificate(certificate)
    if certificate.host_id != peer_host_id:
        raise BrokerError.internal()
    row = connection.execute(
        """
        SELECT peer_host_id, certificate_pem FROM peer_credentials
        WHERE fingerprint = ?
        """,
        (certificate.fingerprint,),
    ).fetchone()
    if row is None:
        connection.execute(
            """
            INSERT INTO peer_credentials (
                fingerprint, peer_host_id, certificate_pem, first_seen_at
            ) VALUES (?, ?, ?, ?)
            """,
            (
                certificate.fingerprint,
                peer_host_id,
                certificate.pem,
                _now(),
            ),
        )
        return
    if (
        len(row) != 2
        or not isinstance(row[0], str)
        or not isinstance(row[1], bytes)
    ):
        raise BrokerError.internal()
    try:
        existing = certificate_material(row[1])
    except BrokerError as error:
        raise BrokerError.internal() from error
    if (
        row[0] != peer_host_id
        or existing.host_id != peer_host_id
        or not secrets.compare_digest(existing.fingerprint, certificate.fingerprint)
        or existing.pem != certificate.pem
    ):
        raise BrokerError.conflict()


def _record_credential_tombstone(
    connection: sqlite3.Connection, relationship: Relationship
) -> None:
    _record_credential_binding(
        connection,
        peer_host_id=relationship.peer_host_id,
        certificate=relationship.certificate,
    )
    certificate = relationship.certificate
    row = connection.execute(
        """
        SELECT peer_host_id FROM credential_tombstones
        WHERE fingerprint = ?
        """,
        (certificate.fingerprint,),
    ).fetchone()
    if row is not None:
        if not isinstance(row[0], str) or row[0] != relationship.peer_host_id:
            raise BrokerError.internal()
        return
    connection.execute(
        """
        INSERT INTO credential_tombstones (fingerprint, peer_host_id, revoked_at)
        VALUES (?, ?, ?)
        """,
        (certificate.fingerprint, relationship.peer_host_id, _now()),
    )


def _active_grants(
    connection: sqlite3.Connection,
    *,
    peer_host_id: str,
    peer_role: Role,
    certificate_fingerprint: str,
    scope: str,
) -> tuple[Relationship, tuple[Grant, ...]]:
    if peer_role is not Role.COORDINATOR or scope not in _GRANT_SCOPES:
        raise BrokerError.unauthorized()
    relationship = _select_relationship(connection, peer_host_id)
    if (
        relationship is None
        or relationship.status != "paired"
        or relationship.peer_role is not Role.COORDINATOR
        or not secrets.compare_digest(
            relationship.certificate.fingerprint, certificate_fingerprint
        )
    ):
        raise BrokerError.unauthorized()
    tombstone = connection.execute(
        """
        SELECT peer_host_id FROM credential_tombstones
        WHERE fingerprint = ?
        """,
        (relationship.certificate.fingerprint,),
    ).fetchone()
    if tombstone is not None:
        if not isinstance(tombstone[0], str):
            raise BrokerError.internal()
        raise BrokerError.unauthorized()
    rows = connection.execute(
        """
        SELECT scope, workspace_id, thread_id, expires_at
        FROM grants
        WHERE peer_host_id = ? AND revision = ? AND scope = ?
          AND (expires_at IS NULL OR expires_at > ?)
        ORDER BY scope, selector_key
        """,
        (
            peer_host_id,
            relationship.grant_revision,
            scope,
            _now(),
        ),
    ).fetchall()
    grants = tuple(
        Grant(
            scope=str(row[0]),
            workspace_id=str(row[1]) if row[1] is not None else None,
            thread_id=str(row[2]) if row[2] is not None else None,
            expires_at=int(row[3]) if row[3] is not None else None,
        )
        for row in rows
    )
    if not grants:
        raise BrokerError.unauthorized()
    return relationship, grants


def _validate_grants(grants: tuple[Grant, ...]) -> None:
    if not grants or len(grants) > len(_GRANT_SCOPES):
        raise BrokerError.invalid_request()
    now = _now()
    selectors: set[tuple[str, str, str]] = set()
    for grant in grants:
        if not isinstance(grant, Grant) or grant.scope not in _GRANT_SCOPES:
            raise BrokerError.invalid_request()
        if grant.workspace_id is not None:
            _validate_identifier(grant.workspace_id)
        if grant.thread_id is not None:
            _validate_identifier(grant.thread_id)
        if grant.scope == "workspaceRead" and grant.thread_id is not None:
            raise BrokerError.invalid_request()
        if grant.expires_at is not None and (
            not isinstance(grant.expires_at, int)
            or isinstance(grant.expires_at, bool)
            or not now < grant.expires_at <= now + 3650 * 86_400
        ):
            raise BrokerError.invalid_request()
        if grant.scope in _WRITE_SCOPES and grant.expires_at is None:
            raise BrokerError.invalid_request()
        key = (
            grant.scope,
            grant.workspace_id or "",
            grant.thread_id or "",
        )
        if key in selectors:
            raise BrokerError.invalid_request()
        selectors.add(key)


def _selector_key(workspace_id: str | None, thread_id: str | None) -> str:
    return f"{workspace_id or ''}\x1f{thread_id or ''}"


def _validate_identifier(value: str) -> None:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > MAX_ID_LENGTH
        or any(character.isspace() or ord(character) < 33 for character in value)
    ):
        raise BrokerError.invalid_request()


def export_existing_audit(
    directory: str | os.PathLike[str], *, limit: int
) -> tuple[dict[str, object], ...]:
    """Read retained redacted audit rows without contacting the controller."""

    _validate_audit_export_limit(limit)
    state_directory = Path(directory)
    if not state_directory.is_absolute():
        raise BrokerError.invalid_request()
    try:
        state_directory.lstat()
    except FileNotFoundError:
        return ()
    except OSError as error:
        raise BrokerError.unavailable() from error
    _validate_private_directory(state_directory)
    database_path = state_directory / STATE_NAME
    if not database_path.exists():
        return ()
    _validate_private_existing_file(database_path)
    try:
        connection = sqlite3.connect(
            f"{database_path.as_uri()}?mode=ro",
            uri=True,
            timeout=1.0,
        )
    except sqlite3.Error as error:
        raise BrokerError.unavailable() from error
    try:
        retention = _read_persisted_audit_retention(connection)
        cutoff = _now() - retention * 86_400
        rows = connection.execute(
            """
            SELECT occurred_at, actor_host_id, peer_host_id, action, scope, result, reason
            FROM audit
            WHERE occurred_at >= ?
            ORDER BY id DESC
            LIMIT ?
            """,
            (cutoff, limit),
        ).fetchall()
    except sqlite3.Error as error:
        raise BrokerError.unavailable() from error
    finally:
        connection.close()
    return tuple(_audit_export_row(row) for row in reversed(rows))


def _validate_audit_export_limit(limit: int) -> None:
    if (
        not isinstance(limit, int)
        or isinstance(limit, bool)
        or not 0 < limit <= MAX_AUDIT_EXPORT_ROWS
    ):
        raise BrokerError.invalid_request()


def _read_persisted_audit_retention(connection: sqlite3.Connection) -> int:
    row = connection.execute(
        "SELECT value FROM metadata WHERE key = ?",
        (_AUDIT_RETENTION_DAYS_KEY,),
    ).fetchone()
    if row is None:
        raise BrokerError.unavailable()
    value = row[0] if len(row) == 1 else None
    try:
        retention = int(value)
    except (TypeError, ValueError) as error:
        raise BrokerError.unavailable() from error
    if not 0 < retention <= 3_650:
        raise BrokerError.unavailable()
    return retention


def _audit_export_row(row: tuple[object, ...]) -> dict[str, object]:
    if len(row) != 7:
        raise BrokerError.unavailable()
    occurred_at, actor_host_id, peer_host_id, action, scope, result, reason = row
    if (
        not isinstance(occurred_at, int)
        or isinstance(occurred_at, bool)
        or occurred_at < 0
        or not isinstance(action, str)
        or not isinstance(result, str)
    ):
        raise BrokerError.unavailable()
    return {
        "occurredAt": occurred_at,
        "actorHostId": _redacted_audit_optional(actor_host_id, 128),
        "peerHostId": _redacted_audit_optional(peer_host_id, 128),
        "action": _redacted_audit_text(action, 128),
        "scope": _redacted_audit_optional(scope, 256),
        "result": _redacted_audit_text(result, 32),
        "reason": _redacted_audit_optional(reason, 128),
    }


def _is_redacted_audit_text(value: object, limit: int) -> bool:
    return (
        isinstance(value, str)
        and bool(value)
        and len(value) <= limit
        and all(
            character.isascii()
            and (character.isalnum() or character in "._,-")
            for character in value
        )
    )


def _redacted_audit_text(value: str, limit: int) -> str:
    return value if _is_redacted_audit_text(value, limit) else "redacted"


def _redacted_audit_optional(value: object, limit: int) -> str | None:
    return value if _is_redacted_audit_text(value, limit) else None


def _validate_private_directory(directory: Path) -> None:
    try:
        info = directory.lstat()
    except FileNotFoundError:
        raise BrokerError.unavailable()
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        not stat.S_ISDIR(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
    ):
        raise BrokerError.unauthorized()


def _validate_private_existing_file(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        raise BrokerError.unavailable()
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
    ):
        raise BrokerError.unauthorized()


def _prepare_private_directory(directory: Path) -> None:
    _reject_symlink_components(directory.parent)
    try:
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = directory.lstat()
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        not stat.S_ISDIR(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
    ):
        raise BrokerError.unauthorized()
    _chmod_private(directory, 0o700)


def _prepare_private_file(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        try:
            descriptor = os.open(path, flags, 0o600)
            os.close(descriptor)
        except FileExistsError:
            return _prepare_private_file(path)
        except PermissionError as error:
            raise BrokerError.unauthorized() from error
        except OSError as error:
            raise BrokerError.unavailable() from error
        return
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
    ):
        raise BrokerError.unauthorized()
    _chmod_private(path, 0o600)


def _secure_sqlite_files(database_path: Path) -> None:
    for path in (
        database_path,
        database_path.with_name(f"{database_path.name}-wal"),
        database_path.with_name(f"{database_path.name}-shm"),
    ):
        try:
            info = path.lstat()
        except FileNotFoundError:
            continue
        except OSError as error:
            raise BrokerError.unavailable() from error
        if (
            stat.S_ISLNK(info.st_mode)
            or not stat.S_ISREG(info.st_mode)
            or _wrong_owner(info)
            or (os.name != "nt" and info.st_mode & 0o077)
        ):
            raise BrokerError.unauthorized()
        _chmod_private(path, 0o600)


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


def _wrong_owner(info: os.stat_result) -> bool:
    return hasattr(os, "getuid") and info.st_uid != os.getuid()


def _chmod_private(path: Path, mode: int) -> None:
    try:
        os.chmod(path, mode, follow_symlinks=False)
    except (NotImplementedError, OSError) as error:
        if os.name != "nt":
            raise BrokerError.unavailable() from error


def _now() -> int:
    return int(time.time())
