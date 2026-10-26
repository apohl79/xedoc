"""Gate new session work while the broker prepares to shut down."""

from contextlib import contextmanager
import threading
from typing import Iterator

from .errors import BrokerError


class WorkAdmissionGate:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._accepting = True
        self._in_flight = 0

    @contextmanager
    def admit(self) -> Iterator[None]:
        with self._lock:
            if not self._accepting:
                raise BrokerError.unavailable()
            self._in_flight += 1
        try:
            yield
        finally:
            with self._lock:
                self._in_flight -= 1

    def close(self) -> None:
        with self._lock:
            self._accepting = False

    def open(self) -> None:
        with self._lock:
            self._accepting = True

    @property
    def in_flight(self) -> int:
        with self._lock:
            return self._in_flight
