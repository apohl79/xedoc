"""Bounded in-memory presence tracking for discovered and paired hosts."""

from __future__ import annotations

from dataclasses import dataclass
import threading
import time
from typing import Callable


MAX_PRESENCE_ENTRIES = 512


@dataclass(frozen=True)
class HostPresence:
    """Point-in-time liveness for one host."""

    last_seen_at: int | None
    reachable: bool | None
    expires_in_seconds: float | None


class PresenceTracker:
    """Track when hosts were last seen without performing any network I/O.

    An entry with a TTL (an unauthenticated announcement) expires on its own;
    an entry without one (a verified describe) stays until it is replaced.
    """

    def __init__(
        self,
        *,
        monotonic: Callable[[], float] = time.monotonic,
        wall: Callable[[], float] = time.time,
    ) -> None:
        self._monotonic = monotonic
        self._wall = wall
        self._lock = threading.Lock()
        self._entries: dict[str, tuple[int | None, bool | None, float | None]] = {}

    def seen(self, host_id: str, *, ttl_seconds: float | None) -> bool:
        """Record a sighting; return False when the bounded table is full."""

        now = self._monotonic()
        with self._lock:
            self._prune(now)
            if host_id not in self._entries and len(self._entries) >= MAX_PRESENCE_ENTRIES:
                return False
            expires_at = None if ttl_seconds is None else now + ttl_seconds
            self._entries[host_id] = (int(self._wall()), True, expires_at)
        return True

    def unreachable(self, host_id: str) -> None:
        """Keep the last sighting but flag the host as not answering."""

        now = self._monotonic()
        with self._lock:
            self._prune(now)
            previous = self._entries.get(host_id)
            last_seen_at = previous[0] if previous is not None else None
            if previous is None and len(self._entries) >= MAX_PRESENCE_ENTRIES:
                return
            self._entries[host_id] = (last_seen_at, False, None)

    def forget(self, host_id: str) -> None:
        with self._lock:
            self._entries.pop(host_id, None)

    def get(self, host_id: str) -> HostPresence | None:
        now = self._monotonic()
        with self._lock:
            self._prune(now)
            entry = self._entries.get(host_id)
        if entry is None:
            return None
        last_seen_at, reachable, expires_at = entry
        return HostPresence(
            last_seen_at=last_seen_at,
            reachable=reachable,
            expires_in_seconds=None if expires_at is None else max(0.0, expires_at - now),
        )

    def expired(self, known_host_ids: set[str]) -> set[str]:
        """Return the ids from ``known_host_ids`` that no longer have an entry."""

        now = self._monotonic()
        with self._lock:
            self._prune(now)
            return {host_id for host_id in known_host_ids if host_id not in self._entries}

    def _prune(self, now: float) -> None:
        for host_id, (_, _, expires_at) in tuple(self._entries.items()):
            if expires_at is not None and expires_at <= now:
                del self._entries[host_id]
