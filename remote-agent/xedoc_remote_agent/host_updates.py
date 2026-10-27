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
        result = {"currentVersion": version, "supported": os.name == "posix",
                  "status": "idle", "output": "", "cursor": 0, "reset": False}
        path = self.directory / "state.json"
        if path.exists():
            stored = json.loads(path.read_text())
            output = base64.b64decode(stored.pop("output", ""))
            end = stored["cursor"]
            start = end - len(output)
            reset = cursor < start or cursor > end
            offset = start if reset else cursor
            chunk = output[offset - start:offset - start + 65536]
            result.update(stored, output=base64.b64encode(chunk).decode(),
                          cursor=offset + len(chunk), reset=reset)
            if result["status"] in ("starting", "running"):
                try:
                    os.kill(stored["pid"], 0)
                except ProcessLookupError:
                    result["status"] = "interrupted"
        return result

    def start(self, tag):
        if os.name != "posix":
            raise BrokerError.unavailable("The installer requires a macOS or Linux host")
        checked = self.check()
        if tag != "v" + checked["latestVersion"] or not checked["available"]:
            raise ValueError("Check for a newer release before installing")
        import fcntl

        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        with (self.directory / "lock").open("a") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise BrokerError.conflict("An installer is already running") from None
            # Keep the old runtime and payload paths, not the changing current symlink.
            paths = [str(Path(path).resolve()) for path in sys.path if path]
            code = (
                f"import sys; sys.path[:0] = {paths!r}; "
                "from xedoc_remote_agent.host_update_worker import run; "
                f"run({str(self.home)!r}, {tag!r}, {lock.fileno()})"
            )
            environment = dict(os.environ, XEDOC_HOME=str(self.home))
            environment.pop("XEDOC_NON_INTERACTIVE", None)
            environment.pop("XEDOC_LOCAL_ZIP", None)
            environment.pop("XEDOC_RELEASE_TARGET", None)
            process = subprocess.Popen(
                [str(Path(sys.executable).resolve()), "-c", code],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL, start_new_session=True,
                pass_fds=(lock.fileno(),), env=environment,
                cwd=str(self.directory),
            )
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

    def input(self, data):
        if not isinstance(data, str) or len(data.encode()) > 4096:
            raise ValueError("Installer input exceeds size limit")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
            connection.settimeout(3)
            connection.connect(str(self.directory / "terminal.sock"))
            connection.sendall(data.encode())
            connection.shutdown(socket.SHUT_WR)
            if connection.recv(16) != b"ok":
                raise BrokerError.unavailable("Installer input was not accepted")
