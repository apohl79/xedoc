"""Authenticated same-origin access to the original TUI-owned browser UI."""

import base64
import http.client
import os
from pathlib import Path
import secrets
import select
import shutil
import socket
import threading
import time
from urllib.parse import urlsplit

from .controller import _default_client_factory
from .errors import BrokerError
from .workspaces import load_bootstrap_descriptor


class _Producer:
    def __init__(self, socket_path):
        self.client = _default_client_factory(socket_path, 30.0)
        self.stopped = threading.Event()
        self.thread = None
        self.query_tail = b""
        try:
            self.client.initialize("xedoc-host-gui-producer", "Xedoc Host GUI", "0.1.0")
        except BaseException:
            self.client.close()
            raise
        self.client.set_notification_handler(self.observe)
        self.client.set_server_request_handler(lambda message: None)

    def observe(self, message):
        if message.get("method") == "process/exited":
            self.stopped.set()
        if message.get("method") == "process/outputDelta":
            chunk = base64.b64decode(message["params"]["deltaBase64"])
            query = self.query_tail + chunk
            self.query_tail = query[-3:]
            if b"\x1b[6n" in query:
                self.client.request(
                    "process/writeStdin",
                    {
                        "processHandle": "gui",
                        "deltaBase64": base64.b64encode(b"\x1b[1;1R").decode(),
                    },
                )

    def spawn(self, params):
        self.client.request("process/spawn", params)
        self.client._transport.socket.settimeout(1)
        self.thread = threading.Thread(
            target=self.drain, daemon=True, name="host-gui-terminal"
        )
        self.thread.start()

    def drain(self):
        try:
            while not self.stopped.is_set():
                try:
                    self.client.handle_message(self.client.receive_message())
                except socket.timeout:
                    continue
        except Exception:
            self.stopped.set()
        finally:
            try:
                self.client.request("process/kill", {"processHandle": "gui"})
            except Exception:
                pass
            self.client.close()

    def close(self):
        self.stopped.set()
        if self.thread is not None:
            self.thread.join(timeout=3)
        else:
            self.client.close()


class HostSessionGuis:
    def __init__(self, home: Path, bootstrap_path: Path):
        self.home = home
        self.bootstrap_path = bootstrap_path
        self.entries = {}
        self.producers = {}
        self.lock = threading.RLock()

    def open(self, username, workspace_id, project_name, thread_id, project, url):
        address = urlsplit(url)
        if (
            address.scheme != "http"
            or address.hostname != "127.0.0.1"
            or not address.port
        ):
            raise BrokerError.unavailable("Invalid local GUI address")
        with self.lock:
            for token, entry in list(self.entries.items()):
                if (entry["username"], entry["threadId"]) == (username, thread_id):
                    if entry["address"].netloc == address.netloc and self._connected(
                        address
                    ):
                        return {"guiUrl": f"/session-ui/{token}/#capability={token}"}
                    self.entries.pop(token)
            if not self._connected(address):
                previous = self.producers.pop(thread_id, None)
                if previous is not None:
                    previous.close()
            if len(self.entries) >= 16:
                raise BrokerError.limit_exceeded(
                    "Close a session tab before opening another"
                )
            client = None
            try:
                if not self._connected(address):
                    binary = (
                        self.home
                        / "packages"
                        / "standalone"
                        / "current"
                        / "bin"
                        / ("xedoc.exe" if os.name == "nt" else "xedoc")
                    )
                    executable = (
                        str(binary) if binary.is_file() else shutil.which("xedoc")
                    )
                    if executable is None:
                        raise BrokerError.unavailable("Xedoc executable is unavailable")
                    descriptor = load_bootstrap_descriptor(self.bootstrap_path)
                    client = _Producer(descriptor.socket_path)
                    client.spawn(
                        {
                            "command": [
                                executable,
                                "resume",
                                "--remote",
                                f"unix://{descriptor.socket_path.as_posix()}",
                                thread_id,
                                "--gui-hosted",
                            ],
                            "processHandle": "gui",
                            "cwd": str(project),
                            "tty": True,
                            "size": {"cols": 160, "rows": 48},
                            "timeoutMs": None,
                            "outputBytesCap": None,
                            "env": {
                                "XEDOC_HOME": str(self.home),
                                "TERM": "xterm-256color",
                            },
                        }
                    )
                    deadline = time.monotonic() + 20
                    while not self._connected(address):
                        if client.stopped.is_set() or time.monotonic() >= deadline:
                            raise BrokerError.unavailable(
                                "Original GUI did not start. Use a binary supporting --gui-hosted "
                                "and complete any first-run trust or sign-in screens in the TUI."
                            )
                        time.sleep(0.1)
                token = secrets.token_hex(24)
                self.entries[token] = {
                    "username": username,
                    "workspaceId": workspace_id,
                    "projectName": project_name,
                    "threadId": thread_id,
                    "address": address,
                }
                if client is not None:
                    self.producers[thread_id] = client
                return {"guiUrl": f"/session-ui/{token}/#capability={token}"}
            except BaseException:
                if client is not None:
                    client.close()
                raise

    @staticmethod
    def _connected(address):
        connection = http.client.HTTPConnection(
            address.hostname, address.port, timeout=3
        )
        try:
            connection.request("GET", "/")
            response = connection.getresponse()
            response.read(2_000_000)
            return response.getheader("X-Xedoc-Producer-Connected") == "true"
        except (http.client.HTTPException, OSError):
            return False
        finally:
            connection.close()

    def entry(self, token, account):
        with self.lock:
            entry = self.entries.get(token)
            if (
                entry is None
                or entry["username"] != account["username"]
                or account["role"] != "admin"
                and entry["workspaceId"] not in account["workspaces"]
            ):
                raise BrokerError.not_found()
            return entry

    def close(self, username=None, thread_id=None, *, workspace_id=None):
        with self.lock:
            for token, entry in list(self.entries.items()):
                if workspace_id is not None and entry["workspaceId"] != workspace_id:
                    continue
                if username is not None and (
                    entry["username"] != username
                    or thread_id is not None
                    and entry["threadId"] != thread_id
                ):
                    continue
                self.entries.pop(token)
                if not any(
                    other["threadId"] == entry["threadId"]
                    for other in self.entries.values()
                ):
                    client = self.producers.pop(entry["threadId"], None)
                    if client is not None:
                        client.close()


def proxy_session_gui(handler, entry, token, resource, cookie):
    address = entry["address"]
    prefix = f"/session-ui/{token}"
    if resource == f"session/{token}":
        if (
            handler.headers.get("Origin")
            != handler.server.origin_for_authority(
                handler.headers.get("Host", "")
            )
            or handler.headers.get("Upgrade", "").lower() != "websocket"
        ):
            handler.reply(403, {"error": "Unexpected GUI connection"})
            return
        capability = address.fragment.removeprefix("capability=")
        upstream = socket.create_connection(
            (address.hostname, address.port), timeout=10
        )
        try:
            upstream.sendall(
                (
                    f"GET /session/{capability} HTTP/1.1\r\n"
                    f"Host: {address.netloc}\r\nOrigin: http://{address.netloc}\r\n"
                    "Connection: Upgrade\r\nUpgrade: websocket\r\n"
                    f"Sec-WebSocket-Key: {handler.headers.get('Sec-WebSocket-Key', '')}\r\n"
                    "Sec-WebSocket-Version: 13\r\n\r\n"
                ).encode("ascii")
            )
            response = bytearray()
            while b"\r\n\r\n" not in response:
                data = upstream.recv(4096)
                if not data or len(response) + len(data) > 65536:
                    raise BrokerError.unavailable("GUI handshake failed")
                response.extend(data)
            if not response.startswith(b"HTTP/1.1 101 "):
                raise BrokerError.unavailable("GUI connection was rejected")
            handler.wfile.write(response)
            handler.wfile.flush()
            handler.close_connection = True
            upstream.settimeout(10)
            handler.connection.settimeout(10)
            last_check = 0
            while True:
                if time.monotonic() - last_check >= 2:
                    try:
                        account = handler.server.auth.account(cookie)
                        handler.server.session_guis.entry(token, account)
                    except (BrokerError, PermissionError):
                        break
                    last_check = time.monotonic()
                pending = getattr(handler.connection, "pending", lambda: 0)()
                ready, _, _ = select.select(
                    [upstream, handler.connection], [], [], 0 if pending else 2
                )
                if pending and handler.connection not in ready:
                    ready.append(handler.connection)
                if not ready:
                    continue
                for source in ready:
                    data = source.recv(65536)
                    if not data:
                        return
                    target = handler.connection if source is upstream else upstream
                    target.sendall(data)
        except OSError:
            return
        finally:
            upstream.close()
        return
    if resource not in ("", "webui.js", "webui.css", "favicon.svg"):
        handler.reply(404, {"error": "Not found"})
        return
    connection = http.client.HTTPConnection(address.hostname, address.port, timeout=10)
    try:
        connection.request("GET", f"/{resource}")
        response = connection.getresponse()
        body = response.read(4_000_001)
        if response.status != 200 or len(body) > 4_000_000:
            raise BrokerError.unavailable("GUI asset is unavailable")
        if resource == "":
            body = body.replace(b'="/', f'="{prefix}/'.encode())
        elif resource == "webui.js":
            body = body.replace(
                b"${location.host}/session/",
                f"${{location.host}}{prefix}/session/".encode(),
            ).replace(
                b'"xedoc-webui-capability"',
                f'"xedoc-webui-capability:{token}"'.encode(),
            )
        handler.reply(
            200,
            body,
            content_type=response.getheader("Content-Type"),
            session_frame=True,
        )
    finally:
        connection.close()
