"""Bounded, untrusted address hints for certificate-pinned paired hosts."""

import secrets
import threading

from .peer_discovery import DiscoveryCandidate
from .peer_protocol import PeerTarget
from .peer_state import PeerState, Relationship


class PeerRelocation:
    def __init__(self, state: PeerState) -> None:
        self._state = state
        self._hints: dict[str, DiscoveryCandidate] = {}
        self._lock = threading.Lock()

    def observe(self, record: DiscoveryCandidate) -> bool:
        relationship = self._state.relationship(record.host_id)
        if (
            relationship is None
            or relationship.status not in {"paired", "suspended"}
            or record.ttl_seconds == 0
            or record.role is not relationship.peer_role
            or not secrets.compare_digest(
                record.fingerprint, relationship.certificate.fingerprint
            )
            or record.endpoint.value == relationship.endpoint
        ):
            return False
        with self._lock:
            if len(self._hints) >= 128 and record.host_id not in self._hints:
                return False
            self._hints[record.host_id] = record
        return True

    def take(self, relationship: Relationship) -> PeerTarget | None:
        with self._lock:
            record = self._hints.pop(relationship.peer_host_id, None)
        if (
            record is None
            or record.role is not relationship.peer_role
            or not secrets.compare_digest(
                record.fingerprint, relationship.certificate.fingerprint
            )
        ):
            return None
        return PeerTarget(
            endpoint=record.endpoint,
            certificate=relationship.certificate,
            role=relationship.peer_role,
        )
