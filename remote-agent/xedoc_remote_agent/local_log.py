"""Redacted, bounded owner-local lifecycle logging."""

from __future__ import annotations

import json
import os
from pathlib import Path
import stat
import threading
import time
from typing import Any

from .errors import BrokerError


LOG_NAME = "owner.log"
LOG_SCHEMA_VERSION = 1
MAX_LOG_BYTES = 256 * 1024
MAX_LOG_RECORD_BYTES = 1024
MAX_LOG_RECORDS = 2_048


class LocalLog:
    """Store only lifecycle event codes in one private, bounded JSONL file."""

    def __init__(self, directory: str | os.PathLike[str], *, retention_days: int) -> None:
        self._directory = Path(directory)
        if (
            not self._directory.is_absolute()
            or not isinstance(retention_days, int)
            or isinstance(retention_days, bool)
            or retention_days <= 0
        ):
            raise BrokerError.invalid_request()
        self._retention_days = retention_days
        self._path = self._directory / LOG_NAME
        self._lock = threading.RLock()

    def record(self, event: str, result: str) -> None:
        """Append one validated lifecycle result without recording request data."""

        if event not in {
            "daemon.started",
            "daemon.startFailed",
            "daemon.shutdownRequested",
            "daemon.stopped",
        } or result not in {"ok", "error"}:
            raise BrokerError.invalid_request()
        record = {
            "schemaVersion": LOG_SCHEMA_VERSION,
            "occurredAt": int(time.time()),
            "event": event,
            "result": result,
        }
        with self._lock:
            records = _read_records(self._path, self._retention_days)
            records.append(record)
            records = records[-MAX_LOG_RECORDS:]
            _write_records(self._path, records)


def _read_records(path: Path, retention_days: int) -> list[dict[str, Any]]:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return []
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
    ):
        raise BrokerError.unauthorized()
    try:
        with path.open("rb") as source:
            source.seek(0, os.SEEK_END)
            start = max(source.tell() - MAX_LOG_BYTES, 0)
            source.seek(start)
            raw = source.read(MAX_LOG_BYTES)
    except OSError as error:
        raise BrokerError.unavailable() from error
    if start:
        raw = raw.partition(b"\n")[2]
    cutoff = int(time.time()) - retention_days * 86_400
    records: list[dict[str, Any]] = []
    for line in raw.splitlines():
        if len(line) > MAX_LOG_RECORD_BYTES:
            continue
        try:
            value = json.loads(line)
        except (UnicodeError, json.JSONDecodeError):
            continue
        if _valid_record(value) and value["occurredAt"] >= cutoff:
            records.append(value)
    return records[-MAX_LOG_RECORDS:]


def _write_records(path: Path, records: list[dict[str, Any]]) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    temp = path.with_name(f".{path.name}.tmp")
    try:
        descriptor = os.open(
            temp,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL,
            0o600,
        )
    except FileExistsError:
        try:
            temp.unlink()
        except OSError as error:
            raise BrokerError.unavailable() from error
        return _write_records(path, records)
    except OSError as error:
        raise BrokerError.unavailable() from error
    try:
        with os.fdopen(descriptor, "wb") as output:
            kept = _bounded_tail(records)
            for record in kept:
                output.write(_encode(record))
                output.write(b"\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temp, path)
        if os.name != "nt":
            os.chmod(path, 0o600)
    except OSError as error:
        try:
            temp.unlink()
        except OSError:
            pass
        raise BrokerError.unavailable() from error


def _bounded_tail(records: list[dict[str, Any]]) -> list[dict[str, Any]]:
    total = 0
    kept: list[dict[str, Any]] = []
    for record in reversed(records):
        encoded = _encode(record)
        if total + len(encoded) + 1 > MAX_LOG_BYTES:
            break
        kept.append(record)
        total += len(encoded) + 1
    kept.reverse()
    return kept


def _valid_record(value: object) -> bool:
    return (
        isinstance(value, dict)
        and set(value) == {"schemaVersion", "occurredAt", "event", "result"}
        and value.get("schemaVersion") == LOG_SCHEMA_VERSION
        and isinstance(value.get("occurredAt"), int)
        and not isinstance(value.get("occurredAt"), bool)
        and value.get("event")
        in {
            "daemon.started",
            "daemon.startFailed",
            "daemon.shutdownRequested",
            "daemon.stopped",
        }
        and value.get("result") in {"ok", "error"}
    )


def _encode(value: dict[str, Any]) -> bytes:
    try:
        encoded = json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
    if len(encoded) > MAX_LOG_RECORD_BYTES:
        raise BrokerError.limit_exceeded()
    return encoded


def _wrong_owner(info: os.stat_result) -> bool:
    return hasattr(os, "getuid") and info.st_uid != os.getuid()
