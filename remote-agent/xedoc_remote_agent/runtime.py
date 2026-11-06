"""Process limits and private, bounded, content-free runtime diagnostics."""

import json
import logging
from logging.handlers import RotatingFileHandler
import os
from pathlib import Path
import re
import ssl
import stat
import sys
import threading
import traceback

from .daemon import DaemonPaths, _prepare_state_directory
from .errors import BrokerError


MAX_DIAGNOSTIC_BYTES = 1024 * 1024
DIAGNOSTIC_BACKUPS = 2


def raise_file_descriptor_limit() -> tuple[int, int] | None:
    if os.name == "nt":
        return None
    import resource

    soft, hard = resource.getrlimit(resource.RLIMIT_NOFILE)
    target = 65_536 if hard == resource.RLIM_INFINITY else min(hard, 65_536)
    if soft != resource.RLIM_INFINITY and soft < target:
        resource.setrlimit(resource.RLIMIT_NOFILE, (target, hard))
    return resource.getrlimit(resource.RLIMIT_NOFILE)


class _PrivateRotatingHandler(RotatingFileHandler):
    def _open(self):
        flags = os.O_WRONLY | os.O_APPEND | os.O_CREAT
        flags |= getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
        descriptor = os.open(self.baseFilename, flags, 0o600)
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode) or (
                hasattr(os, "getuid") and info.st_uid != os.getuid()
            ):
                raise BrokerError.unauthorized()
            if os.name != "nt":
                os.fchmod(descriptor, 0o600)
            return os.fdopen(descriptor, "a", encoding="utf-8")
        except BaseException:
            os.close(descriptor)
            raise


class _DiagnosticFormatter(logging.Formatter):
    def format(self, record):
        value = {
            "occurredAt": int(record.created),
            "level": record.levelname,
            "pid": record.process,
            "source": f"{record.module}:{record.lineno}",
            "event": str(record.msg)[:256],
        }
        for key in ("softLimit", "hardLimit"):
            if isinstance(getattr(record, key, None), int):
                value[key] = getattr(record, key)
        status = getattr(record, "apnsStatus", None)
        if type(status) is int and 100 <= status <= 599:
            value["apnsStatus"] = status
        reason = getattr(record, "apnsReason", None)
        if isinstance(reason, str) and re.fullmatch(r"[A-Za-z]{1,64}", reason):
            value["apnsReason"] = reason
        notification_id = getattr(record, "notificationId", None)
        if isinstance(notification_id, str) and re.fullmatch(
            r"[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}", notification_id
        ):
            value["notificationId"] = notification_id
        error = record.exc_info[1] if record.exc_info else None
        if error is None and isinstance(record.args, tuple):
            error = next(
                (item for item in record.args if isinstance(item, BaseException)), None
            )
        errors = []
        while error is not None and len(errors) < 8:
            detail = {"type": type(error).__name__}
            if isinstance(error, OSError):
                detail["errno"] = error.errno
            if isinstance(error, ssl.SSLError):
                detail["reason"] = getattr(error, "reason", None)
            if isinstance(error, BrokerError):
                detail["code"] = error.code.value
            if error.__traceback__ is not None:
                detail["frames"] = [
                    f"{Path(frame.filename).name}:{frame.lineno}:{frame.name}"
                    for frame in traceback.extract_tb(error.__traceback__)[-8:]
                ]
            errors.append(detail)
            error = error.__cause__
        if errors:
            value["errors"] = errors
        return json.dumps(value, separators=(",", ":"))


def configure_runtime(home: str | os.PathLike[str] | None, *, service: str) -> None:
    limit_error = None
    limits = None
    try:
        limits = raise_file_descriptor_limit()
    except (OSError, ValueError) as error:
        limit_error = error
    paths = DaemonPaths.from_home(home)
    _prepare_state_directory(paths.directory)
    filename = paths.directory / f"{service}-diagnostics.log"
    root = logging.getLogger()
    if not any(
        isinstance(handler, _PrivateRotatingHandler)
        and handler.baseFilename == str(filename)
        for handler in root.handlers
    ):
        handler = _PrivateRotatingHandler(
            filename, maxBytes=MAX_DIAGNOSTIC_BYTES, backupCount=DIAGNOSTIC_BACKUPS
        )
        handler.setFormatter(_DiagnosticFormatter())
        handler.addFilter(
            lambda record: (
                record.name == "root" or record.name.startswith("xedoc_remote_agent.")
            )
        )
        root.addHandler(handler)
    root.setLevel(logging.INFO)
    if limit_error is not None:
        logging.warning(
            "Could not raise file descriptor limit",
            exc_info=(type(limit_error), limit_error, limit_error.__traceback__),
        )
    elif limits is not None:
        logging.info(
            "File descriptor limit",
            extra={"softLimit": limits[0], "hardLimit": limits[1]},
        )

    previous_hook = sys.excepthook
    previous_thread_hook = threading.excepthook

    def exception_hook(error_type, error, trace):
        logging.error(
            "Unhandled process exception", exc_info=(error_type, error, trace)
        )
        previous_hook(error_type, error, trace)

    def thread_exception_hook(args):
        logging.error(
            "Unhandled worker exception",
            exc_info=(args.exc_type, args.exc_value, args.exc_traceback),
        )
        previous_thread_hook(args)

    sys.excepthook = exception_hook
    threading.excepthook = thread_exception_hook
