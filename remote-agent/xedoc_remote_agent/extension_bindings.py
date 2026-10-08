"""Ephemeral broker-owned provenance bindings for extension children."""

from __future__ import annotations

from dataclasses import dataclass
import secrets
import threading
import time
from typing import Any, Callable, Mapping

from .errors import BrokerError
from .message_contract import MessageSource, identifier


_MAX_BINDINGS = 1_024
_LEASE_TTL_SECONDS = 3_600.0
_BIND_FIELDS = {"threadId", "extensionId"}


@dataclass(frozen=True)
class _Binding:
    source: MessageSource
    expires_at: float


class ExtensionBindingRegistry:
    """Issue finite local leases for host-provided extension identities."""

    def __init__(
        self, host_id: str, *, clock: Callable[[], float] = time.monotonic
    ) -> None:
        self._host_id = identifier(host_id)
        self._clock = clock
        self._bindings: dict[str, _Binding] = {}
        self._lock = threading.RLock()

    def bind(self, value: Mapping[str, Any]) -> str:
        """Replace a child identity's lease and return a new opaque capability."""

        if not isinstance(value, Mapping) or set(value) != _BIND_FIELDS:
            raise BrokerError.invalid_request()
        source = MessageSource(
            host_id=self._host_id,
            thread_id=identifier(value["threadId"]),
            extension_id=identifier(value["extensionId"]),
        )
        with self._lock:
            self._expire_locked()
            for lease, binding in tuple(self._bindings.items()):
                if binding.source == source:
                    del self._bindings[lease]
            if len(self._bindings) >= _MAX_BINDINGS:
                raise BrokerError.limit_exceeded()
            lease = f"lease_{secrets.token_urlsafe(32)}"
            self._bindings[lease] = _Binding(
                source=source, expires_at=self._clock() + _LEASE_TTL_SECONDS
            )
            return lease

    def resolve(self, lease: Any) -> MessageSource:
        """Resolve an unexpired lease without accepting caller-supplied source data."""

        lease = identifier(lease)
        with self._lock:
            self._expire_locked()
            binding = self._bindings.get(lease)
            if binding is None:
                raise BrokerError.unauthorized()
            return binding.source

    def revoke(self, lease: Any) -> None:
        """Drop a lease at normal child shutdown."""

        lease = identifier(lease)
        with self._lock:
            self._expire_locked()
            if self._bindings.pop(lease, None) is None:
                raise BrokerError.unauthorized()

    def clear(self) -> None:
        """Fail closed when the IPC server stops."""

        with self._lock:
            self._bindings.clear()

    def _expire_locked(self) -> None:
        now = self._clock()
        for lease, binding in tuple(self._bindings.items()):
            if binding.expires_at <= now:
                del self._bindings[lease]
