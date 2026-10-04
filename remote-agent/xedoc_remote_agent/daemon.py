"""Lifecycle owner for the local-only remote-agent broker.

The daemon owns exactly one controller connection and one private IPC server
for its lifetime.  Its lock and PID files are only coordination state: the
daemon never sends a signal to a PID read from disk.
"""

from __future__ import annotations

from dataclasses import dataclass
import errno
import json
import os
from pathlib import Path
import secrets
import signal
import stat
import threading
from typing import Any, Callable, Mapping

from .controller import BrokerController
from .errors import BrokerError, ErrorCode, map_controller_error
from .ipc import (
    CAPABILITY_NAME,
    SOCKET_NAME,
    LocalIpcClient,
    LocalIpcServer,
)
from .local_log import LOG_NAME
from .local_log import LocalLog
from .messages import MessageService
from .operations import SessionOperations
from .peer import PeerServer, PeerService
from .peer_sessions import PeerSessionOperations
from .peer_state import PeerState
from .peer_state import export_existing_audit
from .workspaces import controller_identifier
from .workspaces import load_bootstrap_descriptor
from .workspaces import selected_bootstrap_path


PID_NAME = "broker.pid"
LOCK_NAME = "broker.lock"
STATE_VERSION = 1
MAX_PID_STATE_BYTES = 4096
MAX_DOCTOR_BYTES = 16 * 1024
MAX_OWNER_IPC_BYTES = 64 * 1024

ControllerFactory = Callable[..., Any]
ClientFactory = Callable[[Path, float], Any]


@dataclass(frozen=True)
class DaemonPaths:
    """Private state paths derived from one absolute XEDOC home."""

    xedoc_home: Path
    directory: Path
    bootstrap_path: Path
    pid_path: Path
    lock_path: Path
    socket_path: Path
    capability_path: Path
    log_path: Path

    @classmethod
    def from_home(cls, xedoc_home: str | os.PathLike[str] | None) -> "DaemonPaths":
        home = _resolve_home(xedoc_home)
        directory = home / "remote-agent"
        return cls(
            xedoc_home=home,
            directory=directory,
            bootstrap_path=selected_bootstrap_path(xedoc_home=home),
            pid_path=directory / PID_NAME,
            lock_path=directory / LOCK_NAME,
            socket_path=directory / SOCKET_NAME,
            capability_path=directory / CAPABILITY_NAME,
            log_path=directory / LOG_NAME,
        )


@dataclass(frozen=True)
class _PidState:
    pid: int
    owner: str


class _ExclusiveLock:
    """Small cross-platform non-blocking advisory lock."""

    def __init__(self, path: Path, *, create: bool = True) -> None:
        self.path = path
        self.create = create
        self._fd: int | None = None

    def acquire(self) -> None:
        if self._fd is not None:
            return
        _validate_or_create_private_file(self.path, create=self.create)
        flags = os.O_RDWR
        if self.create:
            flags |= os.O_CREAT
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        try:
            fd = os.open(self.path, flags, 0o600)
        except FileNotFoundError as error:
            raise BrokerError.unavailable() from error
        except PermissionError as error:
            raise BrokerError.unauthorized() from error
        except OSError as error:
            raise BrokerError.unavailable() from error
        try:
            _chmod_private(self.path, 0o600)
            if os.name == "nt":
                self._lock_windows(fd)
            else:
                self._lock_unix(fd)
        except BrokerError:
            os.close(fd)
            raise
        except BlockingIOError as error:
            os.close(fd)
            raise BrokerError.conflict() from error
        except OSError as error:
            os.close(fd)
            if error.errno in (errno.EACCES, errno.EAGAIN, errno.EWOULDBLOCK):
                raise BrokerError.conflict() from error
            raise BrokerError.unavailable() from error
        self._fd = fd

    def release(self) -> None:
        fd, self._fd = self._fd, None
        if fd is None:
            return
        try:
            if os.name == "nt":
                import msvcrt

                os.lseek(fd, 0, os.SEEK_SET)
                msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
            else:
                import fcntl

                fcntl.flock(fd, fcntl.LOCK_UN)
        except (OSError, ValueError):
            pass
        finally:
            try:
                os.close(fd)
            except OSError:
                pass

    def _lock_unix(self, fd: int) -> None:
        import fcntl

        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)

    def _lock_windows(self, fd: int) -> None:
        import msvcrt

        if os.fstat(fd).st_size == 0:
            os.write(fd, b"\0")
        os.lseek(fd, 0, os.SEEK_SET)
        msvcrt.locking(fd, msvcrt.LK_NBLCK, 1)


class RemoteAgentDaemon:
    """Own one initialized controller and the private local IPC server."""

    def __init__(
        self,
        *,
        xedoc_home: str | os.PathLike[str] | None = None,
        timeout: float = 30.0,
        controller_factory: ControllerFactory | None = None,
        client_factory: ClientFactory | None = None,
        version: str | None = None,
        host_id: str | None = None,
    ) -> None:
        if (
            not isinstance(timeout, (int, float))
            or isinstance(timeout, bool)
            or timeout <= 0
            or timeout > 120
        ):
            raise BrokerError.invalid_request()
        if host_id is not None and (
            not isinstance(host_id, str) or not host_id or len(host_id) > 128
        ):
            raise BrokerError.invalid_request()
        self.paths = DaemonPaths.from_home(xedoc_home)
        self.timeout = float(timeout)
        self.controller_factory = controller_factory or BrokerController.connect
        self.client_factory = client_factory
        self.version = version
        self.host_id = host_id
        self._effective_host_id: str | None = None
        self._lifecycle_lock = threading.RLock()
        self._shutdown = threading.Event()
        self._controller: Any | None = None
        self._operations: SessionOperations | None = None
        self._message_service: MessageService | None = None
        self._ipc: LocalIpcServer | None = None
        self._peer_service: PeerService | None = None
        self._peer_server: PeerServer | None = None
        self._state: PeerState | None = None
        self._local_log: LocalLog | None = None
        self._lock: _ExclusiveLock | None = None
        self._pid_owner: str | None = None
        self._shutdown_reaper: threading.Thread | None = None

    @property
    def running(self) -> bool:
        with self._lifecycle_lock:
            return self._ipc is not None and self._ipc.running

    @property
    def controller(self) -> Any | None:
        return self._controller

    @property
    def ipc(self) -> LocalIpcServer | None:
        return self._ipc

    @property
    def operations(self) -> SessionOperations | None:
        return self._operations

    @property
    def peer_service(self) -> PeerService | None:
        return self._peer_service

    def start(self) -> "RemoteAgentDaemon":
        """Start once in this process; repeated calls are idempotent."""

        with self._lifecycle_lock:
            if self._ipc is not None:
                if self._ipc.running:
                    return self
                if not self._ipc.shutdown_complete:
                    raise BrokerError.unavailable()
                self._finalize_shutdown_locked()
            _prepare_state_directory(self.paths.directory)
            _validate_bootstrap_state(self.paths.bootstrap_path)
            descriptor = load_bootstrap_descriptor(xedoc_home=self.paths.xedoc_home)
            controller_id = controller_identifier(descriptor)
            lock = _ExclusiveLock(self.paths.lock_path)
            lock.acquire()
            owner = secrets.token_urlsafe(24)
            controller: Any | None = None
            ipc: LocalIpcServer | None = None
            peer_service: PeerService | None = None
            message_service: MessageService | None = None
            state: PeerState | None = None
            local_log: LocalLog | None = None
            try:
                _claim_pid(self.paths.pid_path, owner)
                identity = PeerState(
                    self.paths.directory,
                    audit_retention_days=90,
                ).ensure_identity(self.host_id)
                self._effective_host_id = identity.host_id
                controller = self._connect_controller()
                operations, catalog, limits, host_id = _build_components(
                    controller, identity.host_id
                )
                peer_sessions = PeerSessionOperations(
                    controller._connection,
                    controller.workspaces,
                    max_attached_sessions=limits.max_attached_sessions,
                    max_message_bytes=limits.max_message_bytes,
                    max_result_bytes=limits.max_result_bytes,
                    max_wait_seconds=limits.max_wait_seconds,
                )
                state = PeerState(
                    self.paths.directory,
                    audit_retention_days=limits.audit_retention_days,
                )
                local_log = LocalLog(
                    self.paths.directory,
                    retention_days=limits.audit_retention_days,
                )
                identity = state.ensure_identity(identity.host_id)
                if identity.host_id != host_id:
                    raise BrokerError.conflict()
                message_service = MessageService(
                    str(self.paths.directory),
                    controller._connection,
                    controller.workspaces,
                    state,
                    host_id=host_id,
                    max_attached_sessions=limits.max_attached_sessions,
                    max_message_bytes=limits.max_message_bytes,
                    max_result_bytes=limits.max_result_bytes,
                )
                peer_service = PeerService(
                    identity=identity,
                    state=state,
                    config=controller.config,
                    catalog=catalog,
                    session_operations=peer_sessions,
                    message_service=message_service,
                )
                message_service.set_peer_service(peer_service)
                message_service.start()
                peer_server = peer_service.start_listener()
                ipc = LocalIpcServer(
                    catalog,
                    operations,
                    host_id=host_id,
                    max_message_bytes=limits.max_message_bytes,
                    max_result_bytes=limits.max_result_bytes,
                    peer_service=peer_service,
                    message_service=message_service,
                    shutdown_callback=self.request_shutdown,
                    xedoc_home=self.paths.xedoc_home,
                    controller_id=controller_id,
                )
                ipc.start()
            except BrokerError:
                if local_log is not None:
                    _record_log(local_log, "daemon.startFailed", "error")
                if ipc is not None:
                    ipc.stop()
                if peer_service is not None:
                    peer_service.stop_listener()
                if message_service is not None:
                    message_service.stop()
                _close_controller(controller)
                _release_pid(self.paths.pid_path, owner)
                lock.release()
                raise
            except BaseException as error:
                if local_log is not None:
                    _record_log(local_log, "daemon.startFailed", "error")
                if ipc is not None:
                    ipc.stop()
                if peer_service is not None:
                    peer_service.stop_listener()
                if message_service is not None:
                    message_service.stop()
                _close_controller(controller)
                _release_pid(self.paths.pid_path, owner)
                lock.release()
                raise map_controller_error(error) from error
            self._controller = controller
            self._operations = operations
            self._message_service = message_service
            self._ipc = ipc
            self._peer_service = peer_service
            self._peer_server = peer_server
            self._state = state
            self._local_log = local_log
            self._lock = lock
            self._pid_owner = owner
            self._shutdown_reaper = None
            self._shutdown.clear()
            if local_log is not None:
                _record_log(local_log, "daemon.started", "ok")
            return self

    def stop(self) -> bool:
        """Stop serving and report whether ownership was fully released."""

        with self._lifecycle_lock:
            self._shutdown.set()
            peer_service = self._peer_service
            if peer_service is not None:
                peer_service.stop_listener()
            ipc = self._ipc
            if ipc is None:
                return True
            try:
                complete = ipc.stop()
            except BaseException:
                complete = False
            if complete:
                self._finalize_shutdown_locked()
                return True
            self._start_shutdown_reaper_locked(ipc)
            return False

    def request_shutdown(self) -> None:
        """Wake a foreground ``serve`` loop without touching a remote PID."""

        self._shutdown.set()
        if self._local_log is not None:
            _record_log(self._local_log, "daemon.shutdownRequested", "ok")

    def wait(self, timeout: float | None = None) -> bool:
        if timeout is not None and (
            not isinstance(timeout, (int, float))
            or isinstance(timeout, bool)
            or timeout < 0
        ):
            raise BrokerError.invalid_request()
        return self._shutdown.wait(timeout)

    def serve(self, *, install_signal_handlers: bool = True) -> int:
        """Serve in the foreground until shutdown or SIGINT/SIGTERM."""

        self.start()
        previous: dict[int, Any] = {}
        exit_code = 0
        try:
            if install_signal_handlers and threading.current_thread() is threading.main_thread():

                def handle_signal(_signum: int, _frame: Any) -> None:
                    self.request_shutdown()

                for signum in (signal.SIGINT, signal.SIGTERM):
                    try:
                        previous[signum] = signal.getsignal(signum)
                        signal.signal(signum, handle_signal)
                    except (OSError, ValueError):
                        continue
            while self.running and not self._shutdown.wait(0.25):
                pass
        except KeyboardInterrupt:
            pass
        finally:
            for signum, handler in previous.items():
                try:
                    signal.signal(signum, handler)
                except (OSError, ValueError):
                    pass
            if not self.stop():
                exit_code = 1
        return exit_code

    def _start_shutdown_reaper_locked(self, ipc: LocalIpcServer) -> None:
        reaper = self._shutdown_reaper
        if reaper is not None and reaper.is_alive():
            return
        self._shutdown_reaper = threading.Thread(
            target=self._await_shutdown,
            args=(ipc,),
            name="xedoc-remote-agentd-shutdown",
            daemon=True,
        )
        self._shutdown_reaper.start()

    def _await_shutdown(self, ipc: LocalIpcServer) -> None:
        ipc.wait_shutdown()
        with self._lifecycle_lock:
            if self._ipc is ipc and ipc.shutdown_complete:
                self._finalize_shutdown_locked()

    def _finalize_shutdown_locked(self) -> None:
        self._ipc = None
        peer_service, self._peer_service = self._peer_service, None
        self._peer_server = None
        if peer_service is not None:
            peer_service.stop_listener()
        controller, self._controller = self._controller, None
        self._operations = None
        message_service, self._message_service = self._message_service, None
        if message_service is not None:
            message_service.stop()
        lock, self._lock = self._lock, None
        owner, self._pid_owner = self._pid_owner, None
        local_log, self._local_log = self._local_log, None
        self._state = None
        self._shutdown_reaper = None
        _close_controller(controller)
        if owner is not None:
            _release_pid(self.paths.pid_path, owner)
        if lock is not None:
            lock.release()
        if local_log is not None:
            _record_log(local_log, "daemon.stopped", "ok")

    def _connect_controller(self) -> Any:
        host_id = self._effective_host_id
        if host_id is None:
            raise BrokerError.internal()
        kwargs: dict[str, Any] = {
            "xedoc_home": self.paths.xedoc_home,
            "timeout": self.timeout,
            "host_id": host_id,
        }
        if self.client_factory is not None:
            kwargs["client_factory"] = self.client_factory
        if self.version is not None:
            kwargs["version"] = self.version
        return self.controller_factory(**kwargs)

    @classmethod
    def doctor(
        cls,
        *,
        xedoc_home: str | os.PathLike[str] | None = None,
        timeout: float = 30.0,
        controller_factory: ControllerFactory | None = None,
        client_factory: ClientFactory | None = None,
        version: str | None = None,
        host_id: str | None = None,
    ) -> dict[str, Any]:
        """Run bounded, redacted startup and state diagnostics."""

        try:
            daemon = cls(
                xedoc_home=xedoc_home,
                timeout=timeout,
                controller_factory=controller_factory,
                client_factory=client_factory,
                version=version,
                host_id=host_id,
            )
        except BrokerError as error:
            return _doctor_failure("arguments", error)

        checks: dict[str, dict[str, Any]] = {}
        paths = daemon.paths
        try:
            _prepare_state_directory(paths.directory)
            checks["state"] = _doctor_state_check(paths)
        except BrokerError as error:
            checks["state"] = _doctor_error(error)

        try:
            _validate_bootstrap_state(paths.bootstrap_path)
            load_bootstrap_descriptor(xedoc_home=paths.xedoc_home)
            checks["bootstrap"] = {"ok": True, "status": "available"}
        except BrokerError as error:
            checks["bootstrap"] = _doctor_error(error)

        controller: Any | None = None
        limits: Any | None = None
        if checks.get("bootstrap", {}).get("ok") is True:
            try:
                daemon._effective_host_id = PeerState(
                    paths.directory,
                    audit_retention_days=90,
                ).ensure_identity(daemon.host_id).host_id
                controller = daemon._connect_controller()
                config = getattr(controller, "config", None)
                limits = getattr(controller, "limits", None)
                workspaces = getattr(controller, "workspaces", None)
                role = getattr(getattr(config, "role", None), "value", None)
                workspace_count = len(getattr(workspaces, "workspaces", ()))
                config_ok = (
                    role in {"coordinator", "managed"}
                    and workspace_count > 0
                    and limits is not None
                )
                checks["controller"] = {
                    "ok": True,
                    "status": "connected",
                }
                checks["config"] = {
                    "ok": config_ok,
                    "status": "valid" if config_ok else "invalid",
                    "role": role if role in {"coordinator", "managed"} else "unknown",
                    "workspaceCount": min(max(workspace_count, 0), 1024),
                }
            except BrokerError as error:
                checks["controller"] = _doctor_error(error)
                checks["config"] = {"ok": False, "status": "unavailable"}
            except BaseException as error:
                mapped = map_controller_error(error)
                checks["controller"] = _doctor_error(mapped)
                checks["config"] = {"ok": False, "status": "unavailable"}
        else:
            checks["controller"] = {"ok": False, "status": "not_checked"}
            checks["config"] = {"ok": False, "status": "not_checked"}

        try:
            status = cls.status(xedoc_home=paths.xedoc_home)
            if status["status"] == "running":
                if limits is None:
                    checks["ipc"] = {"ok": False, "status": "controller_unavailable"}
                else:
                    client = LocalIpcClient(
                        xedoc_home=paths.xedoc_home,
                        max_message_bytes=int(limits.max_message_bytes),
                        max_result_bytes=int(limits.max_result_bytes),
                        timeout_seconds=min(5.0, daemon.timeout),
                    )
                    client.handshake()
                    checks["ipc"] = {"ok": True, "status": "available"}
            elif status["status"] == "stopped":
                checks["ipc"] = {"ok": True, "status": "stopped"}
            else:
                checks["ipc"] = {"ok": False, "status": status["status"]}
        except BrokerError as error:
            checks["ipc"] = _doctor_error(error)
        except BaseException as error:
            checks["ipc"] = _doctor_error(map_controller_error(error))
        finally:
            _close_controller(controller)

        ok = all(
            isinstance(value, Mapping) and value.get("ok") is True
            for value in checks.values()
        )
        report: dict[str, Any] = {
            "schemaVersion": STATE_VERSION,
            "service": "xedoc-remote-agentd",
            "ok": ok,
            "checks": checks,
        }
        return _bound_json(report)

    @classmethod
    def status(
        cls, *, xedoc_home: str | os.PathLike[str] | None = None
    ) -> dict[str, Any]:
        """Return lifecycle state without reading endpoint or capability data."""

        try:
            paths = DaemonPaths.from_home(xedoc_home)
            pid_present = _pid_metadata_present(paths.pid_path)
            lock_held = _probe_lock(paths.lock_path)
            if lock_held:
                status = "running"
            elif pid_present:
                status = "stale"
            else:
                status = "stopped"
            report = {
                "schemaVersion": STATE_VERSION,
                "service": "xedoc-remote-agentd",
                "status": status,
                "lockHeld": lock_held,
                "pidPresent": pid_present,
            }
            return _bound_json(report)
        except BrokerError as error:
            return _bound_json(
                {
                    "schemaVersion": STATE_VERSION,
                    "service": "xedoc-remote-agentd",
                    "status": "error",
                    "error": {"code": error.code.value},
                }
            )

    @classmethod
    def shutdown(
        cls,
        *,
        xedoc_home: str | os.PathLike[str] | None = None,
        timeout: float = 5.0,
    ) -> dict[str, Any]:
        """Request graceful shutdown through private local broker IPC only."""

        try:
            paths = DaemonPaths.from_home(xedoc_home)
            client = LocalIpcClient(
                xedoc_home=paths.xedoc_home,
                max_message_bytes=MAX_OWNER_IPC_BYTES,
                max_result_bytes=MAX_OWNER_IPC_BYTES,
                timeout_seconds=timeout,
            )
            result = client.call("daemon/shutdown", {}, timeout_seconds=timeout)
            if result != {"status": "shutdownRequested"}:
                raise BrokerError.invalid_request()
            return _bound_json(
                {
                    "schemaVersion": STATE_VERSION,
                    "service": "xedoc-remote-agentd",
                    "status": "shutdownRequested",
                }
            )
        except BrokerError as error:
            return _bound_json(
                {
                    "schemaVersion": STATE_VERSION,
                    "service": "xedoc-remote-agentd",
                    "status": "error",
                    "error": {"code": error.code.value},
                }
            )

    @classmethod
    def audit_export(
        cls,
        *,
        xedoc_home: str | os.PathLike[str] | None = None,
        limit: int = 1_000,
    ) -> tuple[dict[str, object], ...]:
        """Read retained redacted audit records without peer or IPC access."""

        paths = DaemonPaths.from_home(xedoc_home)
        return export_existing_audit(paths.directory, limit=limit)

    @classmethod
    def certificate_export(
        cls,
        *,
        xedoc_home: str | os.PathLike[str] | None = None,
        host_id: str | None = None,
    ) -> str:
        """Materialize and return only this host's public identity certificate."""

        paths = DaemonPaths.from_home(xedoc_home)
        state = PeerState(paths.directory, audit_retention_days=90)
        certificate = state.ensure_identity(host_id).certificate.pem
        try:
            return certificate.decode("ascii")
        except UnicodeDecodeError as error:
            raise BrokerError.internal() from error


# ``Daemon`` is a compact import name for callers that do not need the
# product-specific class name.
Daemon = RemoteAgentDaemon


def _build_components(
    controller: Any, configured_host_id: str
) -> tuple[SessionOperations, Any, Any, str]:
    """Build broker components through controller-owned package-private state."""

    try:
        connection = controller._connection
        catalog = controller._catalog
        workspaces = controller.workspaces
        limits = controller.limits
        host_id = controller.host_id
    except AttributeError as error:
        raise BrokerError.internal() from error
    if not isinstance(host_id, str) or not host_id:
        host_id = configured_host_id
    operations = SessionOperations(
        connection,
        workspaces,
        entity_id=host_id,
        max_attached_sessions=limits.max_attached_sessions,
        max_message_bytes=limits.max_message_bytes,
        max_result_bytes=limits.max_result_bytes,
        max_wait_seconds=limits.max_wait_seconds,
    )
    return operations, catalog, limits, host_id


def _resolve_home(value: str | os.PathLike[str] | None) -> Path:
    raw = value if value is not None else os.environ.get("XEDOC_HOME")
    home = Path(raw) if raw is not None else Path.home() / ".xedoc"
    if not home.is_absolute():
        raise BrokerError.invalid_request()
    _reject_symlink_components(home)
    return home


def _prepare_state_directory(directory: Path) -> None:
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


def _validate_bootstrap_state(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
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


def _validate_or_create_private_file(path: Path, *, create: bool) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        if not create:
            raise BrokerError.unavailable()
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


def _read_pid_state(path: Path) -> _PidState | None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return None
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
        or info.st_size > MAX_PID_STATE_BYTES
    ):
        raise BrokerError.unauthorized()
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise BrokerError.invalid_request() from error
    if not isinstance(value, Mapping):
        raise BrokerError.invalid_request()
    pid = value.get("pid")
    owner = value.get("owner")
    if (
        not isinstance(pid, int)
        or isinstance(pid, bool)
        or not 0 < pid <= 2**31 - 1
        or not isinstance(owner, str)
        or not 16 <= len(owner) <= 128
    ):
        raise BrokerError.invalid_request()
    return _PidState(pid=pid, owner=owner)


def _claim_pid(path: Path, owner: str) -> None:
    try:
        _read_pid_state(path)
    except BrokerError as error:
        if error.code is not ErrorCode.INVALID_REQUEST:
            raise
        # A malformed state file is safe to replace only after the lock and
        # private-file checks above have succeeded.
        _unlink_private_file(path)
    else:
        _unlink_private_file(path)
    value = json.dumps(
        {"version": STATE_VERSION, "pid": os.getpid(), "owner": owner},
        separators=(",", ":"),
    )
    if len(value.encode("utf-8")) > MAX_PID_STATE_BYTES:
        raise BrokerError.internal()
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(path, flags, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as output:
            output.write(value)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        _chmod_private(path, 0o600)
    except FileExistsError as error:
        raise BrokerError.conflict() from error
    except PermissionError as error:
        raise BrokerError.unauthorized() from error
    except OSError as error:
        raise BrokerError.unavailable() from error


def _release_pid(path: Path, owner: str) -> None:
    try:
        state = _read_pid_state(path)
    except BrokerError:
        return
    if state is None or state.pid != os.getpid() or state.owner != owner:
        return
    _unlink_private_file(path)


def _pid_metadata_present(path: Path) -> bool:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return False
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
    ):
        raise BrokerError.unauthorized()
    return True


def _unlink_private_file(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    except OSError:
        return
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
    ):
        return
    try:
        path.unlink()
    except OSError:
        return


def _probe_lock(path: Path) -> bool:
    try:
        path.lstat()
    except FileNotFoundError:
        return False
    except OSError as error:
        raise BrokerError.unavailable() from error
    try:
        lock = _ExclusiveLock(path, create=False)
        lock.acquire()
    except BrokerError as error:
        if error.code is ErrorCode.CONFLICT:
            return True
        raise
    else:
        lock.release()
        return False


def _doctor_state_check(paths: DaemonPaths) -> dict[str, Any]:
    checks = {
        "directory": _path_state(paths.directory, expected="directory"),
        "lock": _path_state(paths.lock_path, expected="file"),
        "pid": _path_state(paths.pid_path, expected="file"),
        "socket": _path_state(paths.socket_path, expected="socket"),
        "capability": _path_state(paths.capability_path, expected="file"),
        "log": _path_state(paths.log_path, expected="file"),
    }
    safe = all(value in {"private", "absent"} for value in checks.values())
    return {
        "ok": safe,
        "status": "private" if safe else "unsafe",
        "entries": checks,
    }


def _path_state(path: Path, *, expected: str) -> str:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return "absent"
    except OSError:
        return "unavailable"
    if stat.S_ISLNK(info.st_mode) or _wrong_owner(info):
        return "unsafe"
    if expected == "directory" and not stat.S_ISDIR(info.st_mode):
        return "unsafe"
    if expected == "file" and not stat.S_ISREG(info.st_mode):
        return "unsafe"
    if expected == "socket" and not stat.S_ISSOCK(info.st_mode):
        return "unsafe"
    if os.name != "nt" and info.st_mode & 0o077:
        return "unsafe"
    return "private"


def _doctor_failure(name: str, error: BrokerError) -> dict[str, Any]:
    return _bound_json(
        {
            "schemaVersion": STATE_VERSION,
            "service": "xedoc-remote-agentd",
            "ok": False,
            "checks": {name: _doctor_error(error)},
        }
    )


def _doctor_error(error: BrokerError) -> dict[str, Any]:
    return {"ok": False, "code": error.code.value}


def _bound_json(value: dict[str, Any]) -> dict[str, Any]:
    try:
        encoded = json.dumps(
            value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
    if len(encoded) > MAX_DOCTOR_BYTES:
        raise BrokerError.limit_exceeded()
    return value


def _record_log(log: LocalLog, event: str, result: str) -> None:
    try:
        log.record(event, result)
    except BrokerError:
        return


def _close_controller(controller: Any | None) -> None:
    if controller is None:
        return
    try:
        close = getattr(controller, "close", None)
        if callable(close):
            close()
    except BaseException:
        return


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
