"""Host GUI terminal tabs backed by app-server PTY processes."""

import base64
import hashlib
import json
import logging
import os
from pathlib import Path
import secrets
import select
import socket
import struct
import threading
import time

from .errors import BrokerError, map_controller_error
from .host_outside_projects import OTHER_WORKSPACE

_TERMINAL_LIMIT = 128
_REPLAY_BYTES = 256 * 1024
_FRAME_LIMIT = 64 * 1024
_ACCESS_INTERVAL = 2.0
_PING_INTERVAL = 30.0
_IDLE_LIMIT = 75.0
_HANDLE = "terminal"
_WEBSOCKET_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"


def _shell_command() -> list[str]:
    if os.name == "nt":
        return [os.environ.get("COMSPEC", "cmd.exe")]
    import pwd

    shell = pwd.getpwuid(os.getuid()).pw_shell or os.environ.get("SHELL") or "/bin/sh"
    return [shell, "-l"]


def _size(cols, rows) -> dict:
    if not all(
        isinstance(value, int) and not isinstance(value, bool) and 1 <= value <= 1000
        for value in (cols, rows)
    ):
        raise ValueError("Terminal size must be between 1 and 1000 cells")
    return {"cols": cols, "rows": rows}


def _wake(watchers) -> None:
    for notifier in watchers:
        try:
            notifier.send(b"\0")
        except OSError:
            pass


class _Terminal:
    def __init__(self, username, workspace_id, project_name, title, client):
        self.id = secrets.token_urlsafe(18)
        self.username = username
        self.workspace_id = workspace_id
        self.project_name = project_name
        self.title = title
        self.client = client
        self.lock = threading.Lock()
        self.send_lock = threading.Lock()
        self.output = bytearray()
        self.end = 0
        self.state = "running"
        self.exit_code = None
        self.watchers = set()
        self.request_id = 0
        client.set_notification_handler(self._notify)

    def public(self) -> dict:
        return {
            "terminalId": self.id,
            "workspaceId": self.workspace_id,
            "projectName": self.project_name,
            "title": self.title,
        }

    def start(self) -> None:
        threading.Thread(
            target=self._drain, daemon=True, name="host-gui-terminal"
        ).start()

    def _notify(self, message) -> None:
        params = message.get("params")
        if not isinstance(params, dict) or params.get("processHandle") != _HANDLE:
            return
        if message.get("method") == "process/outputDelta":
            self._append(base64.b64decode(params["deltaBase64"]))
        elif message.get("method") == "process/exited":
            self._finish("exited", params.get("exitCode"))

    def _drain(self) -> None:
        # This thread is the only reader. Browser input is sent fire-and-forget
        # so it never waits for, or competes with, output reads.
        transport = self.client._transport
        try:
            while self.state == "running":
                # Poll between messages; timed-out partial frames cannot be resumed.
                if not transport.buffer and not select.select(
                    [transport.socket], [], [], 1
                )[0]:
                    continue
                message = self.client.receive_message()
                error = message.get("error")
                if isinstance(error, dict):
                    logging.warning(
                        "Host GUI terminal request failed: %s", error.get("message")
                    )
                self.client.handle_message(message)
        except Exception:
            if self.state == "running":
                logging.warning("Host GUI terminal connection failed", exc_info=True)
        finally:
            self._finish("exited", None)
            with self.send_lock:
                try:
                    self.client.close()
                except OSError:
                    pass

    def _append(self, data: bytes) -> None:
        with self.lock:
            self.output += data
            self.end += len(data)
            del self.output[: max(0, len(self.output) - _REPLAY_BYTES)]
            watchers = list(self.watchers)
        _wake(watchers)

    def _finish(self, state: str, exit_code) -> None:
        with self.lock:
            if self.state != "running":
                return
            self.state = state
            self.exit_code = exit_code
            watchers = list(self.watchers)
        _wake(watchers)

    def read(self, offset: int):
        with self.lock:
            start = self.end - len(self.output)
            reset = None
            if not start <= offset <= self.end:
                reset = offset = start
            return reset, bytes(self.output[offset - start :]), self.end, self.state

    def watch(self, notifier) -> None:
        with self.lock:
            self.watchers.add(notifier)

    def unwatch(self, notifier) -> None:
        with self.lock:
            self.watchers.discard(notifier)

    def _send(self, method: str, params: dict) -> None:
        with self.send_lock:
            if self.state != "running":
                return
            self.request_id += 1
            try:
                self.client._transport.send_json(
                    {
                        "method": method,
                        "id": f"terminal-{self.request_id}",
                        "params": {"processHandle": _HANDLE, **params},
                    }
                )
            except (OSError, RuntimeError):
                logging.warning("Host GUI terminal write failed", exc_info=True)

    def write(self, data: bytes) -> None:
        self._send("process/writeStdin", {"deltaBase64": base64.b64encode(data).decode()})

    def resize(self, size: dict) -> None:
        self._send("process/resizePty", {"size": size})

    def close(self) -> None:
        self._finish("closed", None)
        # Closing the app-server connection kills its connection-scoped process.
        with self.send_lock:
            try:
                self.client._transport.socket.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass


class HostTerminals:
    def __init__(self, workspaces):
        self.workspaces = workspaces
        self.entries: dict[str, _Terminal] = {}
        self.lock = threading.Lock()
        self.sockets = threading.BoundedSemaphore(_TERMINAL_LIMIT)

    def open(self, username, workspace_id, project_name, project: Path, cols, rows):
        size = _size(cols, rows)
        command = _shell_command()
        with self.lock:
            self._prune()
            if len(self.entries) >= _TERMINAL_LIMIT:
                raise BrokerError.limit_exceeded(
                    "Close a terminal tab before opening another"
                )
            client = None
            try:
                client = self.workspaces._new_client(observe_notifications=False)
                terminal = _Terminal(
                    username, workspace_id, project_name, Path(command[0]).name, client
                )
                client.request(
                    "process/spawn",
                    {
                        "command": command,
                        "processHandle": _HANDLE,
                        "cwd": str(project),
                        "tty": True,
                        "outputBytesCap": None,
                        "timeoutMs": None,
                        "env": {"TERM": "xterm-256color", "COLORTERM": "truecolor"},
                        "size": size,
                    },
                )
            except Exception as error:
                if client is not None:
                    client.close()
                raise map_controller_error(error) from error
            self.entries[terminal.id] = terminal
        terminal.start()
        return terminal.public()

    def _prune(self) -> None:
        for terminal_id, terminal in list(self.entries.items()):
            if terminal.state != "running":
                self.entries.pop(terminal_id)

    def _visible(self, account) -> list[_Terminal]:
        visible, revoked = [], []
        with self.lock:
            self._prune()
            for terminal_id, terminal in list(self.entries.items()):
                if terminal.username != account["username"]:
                    continue
                if account["role"] == "admin" or (
                    terminal.workspace_id != OTHER_WORKSPACE
                    and terminal.workspace_id in account["workspaces"]
                ):
                    visible.append(terminal)
                else:
                    revoked.append(self.entries.pop(terminal_id))
        for terminal in revoked:
            terminal.close()
        return visible

    def find(self, terminal_id, account) -> _Terminal:
        for terminal in self._visible(account):
            if terminal.id == terminal_id:
                return terminal
        raise BrokerError.not_found("Terminal is no longer available")

    def list(self, account) -> list[dict]:
        return [terminal.public() for terminal in self._visible(account)]

    def close(self, username=None, terminal_id=None, *, workspace_id=None) -> None:
        with self.lock:
            closed = [
                self.entries.pop(key)
                for key, terminal in list(self.entries.items())
                if (username is None or terminal.username == username)
                and (terminal_id is None or key == terminal_id)
                and (workspace_id is None or terminal.workspace_id == workspace_id)
            ]
        for terminal in closed:
            terminal.close()


class _ProtocolError(Exception):
    def __init__(self, code: int):
        super().__init__(code)
        self.code = code


def serve_terminal_socket(handler, account, cookie, terminal_id, offset) -> None:
    server = handler.server
    if handler.headers.get("Origin") != server.origin_for_authority(
        handler.headers.get("Host", "")
    ):
        handler.reply(403, {"error": "Unexpected terminal connection"})
        return
    key = handler.headers.get("Sec-WebSocket-Key", "")
    if (
        handler.headers.get("Upgrade", "").lower() != "websocket"
        or "upgrade" not in handler.headers.get("Connection", "").lower()
        or handler.headers.get("Sec-WebSocket-Version") != "13"
        or len(base64.b64decode(key, validate=True)) != 16
        or offset < 0
    ):
        raise ValueError("Expected a terminal WebSocket upgrade")
    if not server.terminals.sockets.acquire(blocking=False):
        raise BrokerError.limit_exceeded("Too many open terminal connections")
    try:
        accept = base64.b64encode(
            hashlib.sha1((key + _WEBSOCKET_GUID).encode()).digest()
        ).decode()
        handler.close_connection = True
        handler.wfile.write(
            (
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            ).encode()
        )
        handler.wfile.flush()
        _TerminalSocket(handler.connection).serve(
            server, account, cookie, terminal_id, offset
        )
    except OSError:
        pass
    except Exception:
        logging.exception("Host GUI terminal socket failed")
    finally:
        server.terminals.sockets.release()


class _TerminalSocket:
    def __init__(self, connection):
        self.connection = connection
        self.buffer = bytearray()

    def serve(self, server, account, cookie, terminal_id, offset) -> None:
        try:
            terminal = server.terminals.find(terminal_id, account)
        except BrokerError:
            self._close(4404)
            return
        wake, notifier = socket.socketpair()
        wake.setblocking(False)
        notifier.setblocking(False)
        terminal.watch(notifier)
        try:
            checked = pinged = received = time.monotonic()
            while True:
                reset, data, offset, state = terminal.read(offset)
                if reset is not None:
                    self._send(0x1, json.dumps({"type": "reset", "offset": reset}).encode())
                for start in range(0, len(data), _FRAME_LIMIT):
                    self._send(0x2, data[start : start + _FRAME_LIMIT])
                if state == "closed":
                    raise _ProtocolError(4404)
                if state == "exited":
                    self._send(
                        0x1,
                        json.dumps({"type": "exit", "exitCode": terminal.exit_code}).encode(),
                    )
                    raise _ProtocolError(1000)
                if time.monotonic() - checked >= _ACCESS_INTERVAL:
                    try:
                        server.terminals.find(terminal_id, server.auth.account(cookie))
                    except PermissionError:
                        raise _ProtocolError(1008) from None
                    except BrokerError:
                        raise _ProtocolError(4404) from None
                    checked = time.monotonic()
                # Browsers answer pings, so silence means the peer vanished.
                if time.monotonic() - received >= _IDLE_LIMIT:
                    return
                if time.monotonic() - pinged >= _PING_INTERVAL:
                    self._send(0x9, b"")
                    pinged = time.monotonic()
                # TLS sockets may hold decrypted bytes that select() cannot see.
                if not getattr(self.connection, "pending", lambda: 0)():
                    ready = select.select(
                        [self.connection, wake], [], [], _ACCESS_INTERVAL
                    )[0]
                    if wake in ready:
                        try:
                            wake.recv(4096)
                        except BlockingIOError:
                            pass
                    if self.connection not in ready:
                        continue
                chunk = self.connection.recv(65536)
                if not chunk:
                    return
                received = time.monotonic()
                self.buffer += chunk
                self._receive(terminal)
        except _ProtocolError as error:
            self._close(error.code)
        finally:
            terminal.unwatch(notifier)
            wake.close()
            notifier.close()

    def _receive(self, terminal) -> None:
        while len(self.buffer) >= 2:
            first, second = self.buffer[0], self.buffer[1]
            if first & 0x70 or not first & 0x80 or not second & 0x80:
                raise _ProtocolError(1002)
            length = second & 0x7F
            header = {126: 4, 127: 10}.get(length, 2)
            if len(self.buffer) < header:
                return
            if length == 126:
                length = struct.unpack_from("!H", self.buffer, 2)[0]
            elif length == 127:
                length = struct.unpack_from("!Q", self.buffer, 2)[0]
            if length > _FRAME_LIMIT:
                raise _ProtocolError(1009)
            if len(self.buffer) < header + 4 + length:
                return
            mask = bytes(self.buffer[header : header + 4]) * (length // 4 + 1)
            payload = (
                int.from_bytes(self.buffer[header + 4 : header + 4 + length], "big")
                ^ int.from_bytes(mask[:length], "big")
            ).to_bytes(length, "big")
            del self.buffer[: header + 4 + length]
            opcode = first & 0x0F
            if opcode == 0x2:
                terminal.write(payload)
            elif opcode == 0x1:
                try:
                    message = json.loads(payload)
                    if message.get("type") != "resize":
                        raise ValueError("Unsupported terminal message")
                    terminal.resize(_size(message.get("cols"), message.get("rows")))
                except (ValueError, AttributeError):
                    raise _ProtocolError(1003) from None
            elif opcode == 0x8:
                raise _ProtocolError(
                    struct.unpack("!H", payload[:2])[0] if len(payload) >= 2 else 1000
                )
            elif opcode == 0x9:
                self._send(0xA, payload)
            elif opcode != 0xA:
                raise _ProtocolError(1003)

    def _send(self, opcode: int, payload: bytes) -> None:
        length = len(payload)
        if length < 126:
            header = struct.pack("!BB", 0x80 | opcode, length)
        elif length <= 0xFFFF:
            header = struct.pack("!BBH", 0x80 | opcode, 126, length)
        else:
            header = struct.pack("!BBQ", 0x80 | opcode, 127, length)
        self.connection.sendall(header + payload)

    def _close(self, code: int) -> None:
        try:
            self._send(0x8, struct.pack("!H", code))
        except OSError:
            pass
