"""Owner-only release updates using the existing interactive installer."""

import base64
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import threading
import time
from urllib.request import Request, urlopen

from .errors import BrokerError


REPOSITORY = "apohl79/xedoc"
_TAG = re.compile(r"v(\d+)\.(\d+)\.(\d+)\Z")


def _process_is_running(pid):
    if os.name == "nt":
        from .windows_ipc import process_is_running

        return process_is_running(pid)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def release_bytes(url: str, limit: int) -> bytes:
    request = Request(url, headers={"User-Agent": "Xedoc-Host-Updater"})
    with urlopen(request, timeout=20) as response:
        content = response.read(limit + 1)
    if len(content) > limit:
        raise ValueError("Release response exceeds size limit")
    return content


class HostUpdates:
    def __init__(self, home: Path):
        self.home = home.resolve()
        self.directory = self.home / "host-gui" / "update"
        self.lock = threading.Lock()
        self.release = None
        self.checked_at = 0

    def check(self):
        with self.lock:
            if time.time() - self.checked_at > 300:
                release = json.loads(release_bytes(
                    f"https://api.github.com/repos/{REPOSITORY}/releases/latest",
                    512 * 1024,
                ))
                tag = release.get("tag_name", "")
                if not isinstance(tag, str) or not _TAG.fullmatch(tag):
                    raise ValueError("Latest release has no stable version tag")
                self.release = tag
                self.checked_at = time.time()
            result = self.status()
            current = _TAG.fullmatch("v" + (result["currentVersion"] or ""))
            latest = _TAG.fullmatch(self.release)
            return {
                **result,
                "latestVersion": self.release[1:],
                "available": current is not None
                and tuple(map(int, latest.groups())) > tuple(map(int, current.groups())),
            }

    def status(self, cursor=0):
        if not isinstance(cursor, int) or cursor < 0:
            raise ValueError("Invalid update output cursor")
        current = self.home / "packages" / "standalone" / "current"
        version = None
        try:
            version = json.loads((current / "xedoc-package.json").read_text())["version"]
        except (OSError, ValueError, KeyError):
            pass
        result = {"currentVersion": version, "supported": os.name in ("posix", "nt"),
                  "status": "idle", "output": "", "cursor": 0, "reset": False}
        path = self.directory / "state.json"
        if path.exists():
            stored = json.loads(path.read_text())
            output = base64.b64decode(stored.pop("output", ""))
            stored.pop("_inputPort", None)
            stored.pop("_inputToken", None)
            end = stored["cursor"]
            start = end - len(output)
            reset = cursor < start or cursor > end
            offset = start if reset else cursor
            chunk = output[offset - start:offset - start + 65536]
            result.update(stored, output=base64.b64encode(chunk).decode(),
                          cursor=offset + len(chunk), reset=reset)
            if result["status"] in ("starting", "running"):
                if not _process_is_running(stored["pid"]):
                    result["status"] = "interrupted"
        return result

    def start(self, tag):
        if os.name not in ("posix", "nt"):
            raise BrokerError.unavailable(
                "Updates are supported on macOS, Linux, and Windows hosts"
            )
        checked = self.check()
        if tag != "v" + checked["latestVersion"] or not checked["available"]:
            raise ValueError("Check for a newer release before installing")

        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        with (self.directory / "lock").open("a") as lock:
            if os.name == "nt":
                import msvcrt

                lock.seek(0, os.SEEK_END)
                if lock.tell() == 0:
                    lock.write("\0")
                    lock.flush()
                lock.seek(0)
                try:
                    msvcrt.locking(lock.fileno(), msvcrt.LK_NBLCK, 1)
                except OSError:
                    raise BrokerError.conflict(
                        "An installer is already running"
                    ) from None
                try:
                    state_path = self.directory / "state.json"
                    if state_path.exists():
                        state = json.loads(state_path.read_text())
                        if state.get("status") in ("starting", "running"):
                            if _process_is_running(state["pid"]):
                                raise BrokerError.conflict(
                                    "An installer is already running"
                                )
                    process = self._start_worker(tag, "run_windows", windows=True)
                    temporary = self.directory / "state.tmp"
                    temporary.write_text(
                        json.dumps(
                            {
                                "pid": process.pid,
                                "tag": tag,
                                "status": "starting",
                                "exitCode": None,
                                "cursor": 0,
                                "output": "",
                                "progress": None,
                                "info": "Starting installer…",
                                "prompt": None,
                                "promptId": 0,
                            }
                        )
                    )
                    temporary.chmod(0o600)
                    temporary.replace(self.directory / "state.json")
                finally:
                    lock.seek(0)
                    msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl

                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    raise BrokerError.conflict(
                        "An installer is already running"
                    ) from None
                process = self._start_worker(tag, "run", lock_fd=lock.fileno())

            # Reap the worker without coupling its lifetime to the host service.
            threading.Thread(target=process.wait, daemon=True).start()
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                path = self.directory / "state.json"
                if path.exists() and json.loads(path.read_text()).get("pid") == process.pid:
                    break
                time.sleep(0.02)
            else:
                raise BrokerError.unavailable("Installer worker could not start")
        return {"ok": True}

    def _start_worker(self, tag, entrypoint, *, windows=False, lock_fd=None):
        # Keep old runtime and payload paths, not the changing current junction.
        paths = [str(Path(path).resolve()) for path in sys.path if path]
        call = (
            f"run({str(self.home)!r}, {tag!r}, {lock_fd})"
            if lock_fd is not None
            else f"{entrypoint}({str(self.home)!r}, {tag!r})"
        )
        code = (
            f"import sys; sys.path[:0] = {paths!r}; "
            f"from xedoc_remote_agent.host_update_worker import {entrypoint}; "
            f"{call}"
        )
        environment = dict(os.environ, XEDOC_HOME=str(self.home))
        if windows:
            environment["XEDOC_GUI_UPDATE"] = "1"
        environment.pop("XEDOC_NON_INTERACTIVE", None)
        environment.pop("XEDOC_LOCAL_ZIP", None)
        environment.pop("XEDOC_RELEASE_TARGET", None)
        options = {
            "stdin": subprocess.DEVNULL,
            "stdout": subprocess.DEVNULL,
            "stderr": subprocess.DEVNULL,
            "env": environment,
            "cwd": str(self.directory),
        }
        if windows:
            options["creationflags"] = subprocess.DETACHED_PROCESS
        else:
            options["start_new_session"] = True
            options["pass_fds"] = (lock_fd,)
        return subprocess.Popen(
            [str(Path(sys.executable).resolve()), "-c", code],
            **options,
        )

    def input(self, data):
        if not isinstance(data, str) or len(data.encode()) > 4096:
            raise ValueError("Installer input exceeds size limit")
        if os.name == "nt":
            from .windows_ipc import unprotect

            state = json.loads((self.directory / "state.json").read_text())
            port = state.get("_inputPort")
            token = state.get("_inputToken")
            if not isinstance(port, int) or not isinstance(token, str):
                raise BrokerError.unavailable("Installer input is not available")
            secret = unprotect(base64.b64decode(token))
            message = secret + b"\0" + data.encode()
            with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connection:
                connection.settimeout(3)
                connection.connect(("127.0.0.1", port))
                connection.sendall(message)
                connection.shutdown(socket.SHUT_WR)
                if connection.recv(16) != b"ok":
                    raise BrokerError.unavailable("Installer input was not accepted")
            return
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(3)
            connection.connect(str(self.directory / "terminal.sock"))
            connection.sendall(data.encode())
            connection.shutdown(socket.SHUT_WR)
            if connection.recv(16) != b"ok":
                raise BrokerError.unavailable("Installer input was not accepted")
