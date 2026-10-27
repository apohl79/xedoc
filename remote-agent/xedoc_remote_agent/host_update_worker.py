"""Detached PTY owner that survives app-server and host-GUI restarts."""

import base64
import json
import os
from pathlib import Path
import select
import socket
import struct
import time

from .host_updates import REPOSITORY, release_bytes


def run(home: str, tag: str, lock_fd: int):
    import fcntl
    import pty
    import termios

    directory = Path(home) / "host-gui" / "update"
    os.set_inheritable(lock_fd, False)
    output = b""
    total = 0
    status = "starting"
    exit_code = None

    def save():
        temporary = directory / "state.tmp"
        temporary.write_text(json.dumps({
            "pid": os.getpid(), "tag": tag, "status": status,
            "exitCode": exit_code, "cursor": total,
            "output": base64.b64encode(output).decode(),
        }))
        temporary.chmod(0o600)
        temporary.replace(directory / "state.json")

    def record(chunk):
        nonlocal output, total
        output = (output + chunk)[-262144:]
        total += len(chunk)

    terminal = None
    child = None
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    socket_path = directory / "terminal.sock"
    try:
        save()
        record(f"Fetching installer for {tag}…\r\n".encode())
        save()
        installer = release_bytes(
            f"https://raw.githubusercontent.com/{REPOSITORY}/{tag}/scripts/install/install.sh",
            512 * 1024,
        )
        script = directory / "install.sh"
        script.write_bytes(installer)
        script.chmod(0o600)
        socket_path.unlink(missing_ok=True)
        listener.bind(str(socket_path))
        socket_path.chmod(0o600)
        listener.listen(4)
        child, terminal = pty.fork()
        if child == 0:
            try:
                os.environ["TERM"] = "xterm-256color"
                os.environ["COLUMNS"] = "100"
                os.environ["LINES"] = "30"
                os.execvp("sh", ["sh", str(script), "--repo", REPOSITORY, "--tag", tag])
            finally:
                os._exit(127)
        fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        status = "running"
        save()
        last_save = time.monotonic()
        saved_total = total
        while True:
            ready, _, _ = select.select([terminal, listener], [], [], 0.1)
            if listener in ready:
                connection, _ = listener.accept()
                with connection:
                    try:
                        connection.settimeout(2)
                        chunks = bytearray()
                        while len(chunks) <= 4096:
                            chunk = connection.recv(4097 - len(chunks))
                            if not chunk:
                                break
                            chunks.extend(chunk)
                        if len(chunks) <= 4096:
                            remaining = bytes(chunks)
                            while remaining:
                                remaining = remaining[os.write(terminal, remaining):]
                            connection.sendall(b"ok")
                    except OSError:
                        pass
            if terminal in ready:
                try:
                    chunk = os.read(terminal, 16384)
                except OSError:
                    break
                if not chunk:
                    break
                record(chunk)
            if total != saved_total and time.monotonic() - last_save >= 0.1:
                save()
                last_save = time.monotonic()
                saved_total = total
            completed, wait_status = os.waitpid(child, os.WNOHANG)
            if completed:
                exit_code = os.waitstatus_to_exitcode(wait_status)
                child = None
                # Drain buffered terminal output before completing.
                while select.select([terminal], [], [], 0)[0]:
                    try:
                        chunk = os.read(terminal, 16384)
                    except OSError:
                        break
                    if not chunk:
                        break
                    record(chunk)
                break
        if child is not None:
            _, wait_status = os.waitpid(child, 0)
            exit_code = os.waitstatus_to_exitcode(wait_status)
            child = None
        status = "completed" if exit_code == 0 else "failed"
    except Exception as error:
        record(f"\r\nInstaller failed: {error}\r\n".encode())
        status = "failed"
    finally:
        save()
        listener.close()
        socket_path.unlink(missing_ok=True)
        if terminal is not None:
            os.close(terminal)
        if child is not None:
            os.waitpid(child, 0)
        os.close(lock_fd)
