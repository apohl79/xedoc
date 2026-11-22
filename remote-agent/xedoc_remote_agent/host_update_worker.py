"""Detached PTY owner that survives app-server and host-GUI restarts."""

import base64
import json
import os
from pathlib import Path
import queue
import re
import secrets
import select
import shutil
import socket
import struct
import subprocess
import threading
import time

from .host_updates import REPOSITORY, release_bytes


def run_windows(home: str, tag: str):
    import msvcrt
    from .windows_ipc import protect

    directory = Path(home) / "host-gui" / "update"
    output = b""
    total = 0
    status = "starting"
    exit_code = None
    progress = None
    info = f"Fetching installer for {tag}…"
    prompt = None
    prompt_id = 0
    input_token = secrets.token_bytes(32)
    input_port = None
    lock_file = (directory / "lock").open("a+")
    lock_file.seek(0)
    msvcrt.locking(lock_file.fileno(), msvcrt.LK_LOCK, 1)

    def save():
        temporary = directory / "state.tmp"
        temporary.write_text(
            json.dumps(
                {
                    "pid": os.getpid(),
                    "tag": tag,
                    "status": status,
                    "exitCode": exit_code,
                    "cursor": total,
                    "output": base64.b64encode(output).decode(),
                    "progress": progress,
                    "info": info,
                    "prompt": prompt,
                    "promptId": prompt_id,
                    "_inputPort": input_port,
                    "_inputToken": base64.b64encode(protect(input_token)).decode(),
                }
            )
        )
        temporary.chmod(0o600)
        for attempt in range(20):
            try:
                temporary.replace(directory / "state.json")
                break
            except PermissionError:
                if attempt == 19:
                    raise
                time.sleep(0.01)

    def record(chunk):
        nonlocal output, total, info, prompt, prompt_id
        output = (output + chunk)[-262144:]
        total += len(chunk)
        text = re.sub(
            r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*\x07)",
            "",
            output[-8192:].decode(errors="replace"),
        )
        lines = [
            line.strip()
            for line in text.replace("\r", "\n").split("\n")
            if line.strip()
        ]
        if lines:
            info = lines[-1][:2048]
            question = re.fullmatch(r"(.+?)\s*\[[yYnN]/[yYnN]\]\s*:?\s*", info)
            if question and question[1] != prompt:
                prompt = question[1]
                prompt_id += 1

    child = None
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        save()
        record(f"Fetching installer for {tag}…\r\n".encode())
        save()
        installer = release_bytes(
            f"https://raw.githubusercontent.com/{REPOSITORY}/{tag}/scripts/install/install.ps1",
            512 * 1024,
        )
        redirected_prompt_check = b"-and -not [Console]::IsInputRedirected"
        prompt_check_count = installer.count(redirected_prompt_check)
        prompt_line = b'$answer = Read-Host "$Prompt [y/N]"'
        prompt_line_count = installer.count(prompt_line)
        if prompt_check_count != 1 or prompt_line_count != 1:
            raise ValueError("Windows installer prompt handling changed")
        installer = installer.replace(
            redirected_prompt_check,
            b"-and (-not [Console]::IsInputRedirected -or "
            b"$env:XEDOC_GUI_UPDATE -match '^(1|true|yes)$')",
        )
        installer = installer.replace(
            prompt_line,
            b'Write-Host "$Prompt [y/N]"; $answer = Read-Host',
        )
        script = directory / "install.ps1"
        script.write_bytes(installer)
        script.chmod(0o600)
        listener.bind(("127.0.0.1", 0))
        input_port = listener.getsockname()[1]
        listener.listen(4)
        listener.settimeout(0.1)
        environment = dict(os.environ, XEDOC_HOME=str(home))
        environment.pop("XEDOC_NON_INTERACTIVE", None)
        environment.pop("XEDOC_LOCAL_ZIP", None)
        environment.pop("XEDOC_RELEASE_TARGET", None)
        powershell = shutil.which("powershell.exe") or str(
            Path(os.environ.get("SystemRoot", r"C:\Windows"))
            / "System32"
            / "WindowsPowerShell"
            / "v1.0"
            / "powershell.exe"
        )
        child = subprocess.Popen(
            [
                powershell,
                "-NoLogo",
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                str(script),
                "-Repo",
                REPOSITORY,
                "-Release",
                tag,
            ],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            bufsize=0,
            cwd=str(directory),
            env=environment,
        )
        chunks = queue.Queue()

        def read_output():
            while True:
                chunk = child.stdout.read(16384)
                if not chunk:
                    chunks.put(None)
                    return
                chunks.put(chunk)

        threading.Thread(target=read_output, daemon=True).start()
        status = "running"
        save()
        last_save = time.monotonic()
        saved_state = None
        reader_finished = False
        while True:
            try:
                connection, _ = listener.accept()
            except TimeoutError:
                connection = None
            if connection is not None:
                with connection:
                    try:
                        connection.settimeout(2)
                        data = bytearray()
                        while len(data) <= 4128:
                            chunk = connection.recv(4129 - len(data))
                            if not chunk:
                                break
                            data.extend(chunk)
                        secret = data[:32]
                        separator = data[32:33]
                        input_data = data[33:]
                        if (
                            len(data) <= 4129
                            and len(secret) == 32
                            and separator == b"\0"
                            and secrets.compare_digest(secret, input_token)
                            and len(input_data) <= 4096
                        ):
                            child.stdin.write(input_data)
                            child.stdin.flush()
                            prompt = None
                            connection.sendall(b"ok")
                    except OSError:
                        pass
            while True:
                try:
                    chunk = chunks.get_nowait()
                except queue.Empty:
                    break
                if chunk is None:
                    reader_finished = True
                    break
                record(chunk)
            return_code = child.poll()
            if return_code is not None and reader_finished:
                exit_code = return_code
                break
            state = (total, progress, info, prompt, prompt_id)
            if state != saved_state and time.monotonic() - last_save >= 0.1:
                save()
                last_save = time.monotonic()
                saved_state = state
        status = "completed" if exit_code == 0 else "failed"
        if exit_code == 0:
            progress = 100
    except Exception as error:
        record(f"\r\nInstaller failed: {error}\r\n".encode())
        status = "failed"
    finally:
        prompt = None
        save()
        listener.close()
        if child is not None:
            if child.poll() is None:
                child.terminate()
                child.wait()
            child.stdout.close()
            child.stdin.close()
        lock_file.seek(0)
        msvcrt.locking(lock_file.fileno(), msvcrt.LK_UNLCK, 1)
        lock_file.close()


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
    progress = None
    info = f"Fetching installer for {tag}…"
    prompt = None
    prompt_id = 0
    events, event_writer = os.pipe()
    event_buffer = b""

    def save():
        temporary = directory / "state.tmp"
        temporary.write_text(json.dumps({
            "pid": os.getpid(), "tag": tag, "status": status,
            "exitCode": exit_code, "cursor": total,
            "output": base64.b64encode(output).decode(),
            "progress": progress, "info": info,
            "prompt": prompt, "promptId": prompt_id,
        }))
        temporary.chmod(0o600)
        temporary.replace(directory / "state.json")

    def record(chunk):
        nonlocal output, total, info, prompt, prompt_id
        output = (output + chunk)[-262144:]
        total += len(chunk)
        # Also support prompts from setup subprocesses and older release scripts.
        text = re.sub(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*\x07)", "",
                      output[-8192:].decode(errors="replace"))
        lines = [line.strip() for line in text.replace("\r", "\n").split("\n")
                 if line.strip()]
        if lines:
            info = lines[-1][:2048]
            question = re.fullmatch(r"(.+?)\s*\[[yYnN]/[yYnN]\]\s*:?\s*", info)
            if question and question[1] != prompt:
                prompt = question[1]
                prompt_id += 1

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
                os.close(events)
                os.dup2(event_writer, 3)
                os.set_inheritable(3, True)
                if event_writer != 3:
                    os.close(event_writer)
                os.environ["XEDOC_INSTALL_GUI"] = "1"
                os.environ["TERM"] = "xterm-256color"
                os.environ["COLUMNS"] = "100"
                os.environ["LINES"] = "30"
                os.execvp("sh", ["sh", str(script), "--repo", REPOSITORY, "--tag", tag])
            finally:
                os._exit(127)
        os.close(event_writer)
        event_writer = None
        fcntl.ioctl(terminal, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        status = "running"
        save()
        last_save = time.monotonic()
        saved_state = None
        while True:
            ready, _, _ = select.select(
                [terminal, listener] + ([events] if events is not None else []),
                [], [], 0.1,
            )
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
                            prompt = None
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
            if events in ready:
                chunk = os.read(events, 8192)
                if not chunk:
                    os.close(events)
                    events = None
                else:
                    event_buffer += chunk
                    while b"\n" in event_buffer:
                        line, event_buffer = event_buffer.split(b"\n", 1)
                        kind, _, value = line.decode(errors="replace").partition("\t")
                        if kind == "progress" and value.isdigit():
                            progress = max(progress or 0, min(int(value), 99))
                        elif kind == "info":
                            info = value[:2048]
                        elif kind == "prompt":
                            prompt = value[:2048] or None
                            if prompt:
                                prompt_id += 1
                    event_buffer = event_buffer[-8192:]
            state = (total, progress, info, prompt, prompt_id)
            if state != saved_state and time.monotonic() - last_save >= 0.1:
                save()
                last_save = time.monotonic()
                saved_state = state
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
        if exit_code == 0:
            progress = 100
    except Exception as error:
        record(f"\r\nInstaller failed: {error}\r\n".encode())
        status = "failed"
    finally:
        prompt = None
        save()
        listener.close()
        socket_path.unlink(missing_ok=True)
        if terminal is not None:
            os.close(terminal)
        if events is not None:
            os.close(events)
        if event_writer is not None:
            os.close(event_writer)
        if child is not None:
            os.waitpid(child, 0)
        os.close(lock_fd)
