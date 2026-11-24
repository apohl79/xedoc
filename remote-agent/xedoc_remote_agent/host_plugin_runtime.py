"""Bounded, session-owned plugin runtimes with independent panel instances."""

import hashlib
import json
import os
import queue
import shutil
import subprocess
import sys
import threading

from .errors import BrokerError


PROTOCOL = "xedoc.plugin-panel-runtime/v1"
RESPONSE_LIMIT = 65536
REQUEST_LIMIT = 2 * 1024 * 1024


class RuntimeOpenError(ValueError):
    """One instance failed without invalidating its runtime's command channel."""


def launch_process(home, panel, project, directory, *, reuse=False):
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(directory, 0o700)
    command = [str(panel["executable"])]
    if panel["interpreter"]:
        runtime = (
            sys.executable if panel["interpreter"] == "python" else shutil.which("node")
        )
        if not runtime:
            raise BrokerError.unavailable("The plugin's Node.js runtime is unavailable")
        command.insert(0, runtime)
    environment = {**os.environ, "XEDOC_HOME": str(home), "HARNESS": "xedoc"}
    environment.pop("XEDOC_PLUGIN_PANEL_REUSE", None)
    if reuse:
        environment["XEDOC_PLUGIN_PANEL_REUSE"] = "session"
    binary_directory = home / "packages" / "standalone" / "current" / "bin"
    if binary_directory.is_dir():
        environment["PATH"] = (
            str(binary_directory) + os.pathsep + environment.get("PATH", "")
        )
    with (directory / "runtime.log").open("ab") as log:
        os.chmod(directory / "runtime.log", 0o600)
        return subprocess.Popen(
            command,
            cwd=project,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log,
            env=environment,
        )


def stop_process(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    for stream in (process.stdin, process.stdout):
        if stream is not None:
            try:
                stream.close()
            except (OSError, ValueError):
                pass


class PanelRuntime:
    def __init__(self, process):
        self.process = process
        self.responses = queue.Queue(maxsize=1)
        self.ready = threading.Event()
        self.stopped = threading.Event()
        self.startup_error = None
        threading.Thread(
            target=self._read,
            daemon=True,
            name="plugin-runtime-responses",
        ).start()

    def _line(self):
        line = self.process.stdout.readline(RESPONSE_LIMIT + 1)
        if not line.endswith(b"\n") or len(line) > RESPONSE_LIMIT:
            raise ValueError("Invalid plugin runtime response framing")
        result = json.loads(line)
        if not isinstance(result, dict) or result.get("protocol") != PROTOCOL:
            raise ValueError("Invalid plugin runtime response protocol")
        return result

    def _read(self):
        try:
            if self._line() != {"protocol": PROTOCOL, "ready": True}:
                raise ValueError("Invalid plugin runtime ready response")
        except (OSError, ValueError) as error:
            self.startup_error = error
            self.ready.set()
            return
        self.ready.set()
        try:
            while not self.stopped.is_set():
                if not self._deliver(self._line()):
                    return
        except (OSError, ValueError) as error:
            self._deliver(error)

    def _deliver(self, value):
        while not self.stopped.is_set():
            try:
                self.responses.put(value, timeout=0.1)
                return True
            except queue.Full:
                continue
        return False

    def request(self, op, *, instance_id=None, context=None, timeout=5):
        if not self.ready.wait(timeout) or self.startup_error:
            raise ValueError("Plugin runtime failed to become ready")
        command = {"protocol": PROTOCOL, "op": op}
        if instance_id is not None:
            command["instanceId"] = instance_id
        if context is not None:
            command["context"] = context
        packet = (json.dumps(command, ensure_ascii=False) + "\n").encode()
        if len(packet) > REQUEST_LIMIT:
            raise RuntimeOpenError("Plugin runtime request is too large")
        completed = queue.Queue(maxsize=1)

        def exchange():
            try:
                self.process.stdin.write(packet)
                self.process.stdin.flush()
                result = self.responses.get(timeout=timeout)
            except (OSError, ValueError, queue.Empty) as error:
                result = error
            completed.put(result)

        threading.Thread(
            target=exchange, daemon=True, name="plugin-runtime-command"
        ).start()
        try:
            result = completed.get(timeout=timeout)
        except queue.Empty as error:
            raise ValueError("Plugin runtime command timed out") from error
        if isinstance(result, Exception):
            raise ValueError("Plugin runtime command failed") from result
        if instance_id is not None and result.get("instanceId") != instance_id:
            raise ValueError("Plugin runtime response instance does not match")
        if "error" in result:
            if (
                not isinstance(result["error"], str)
                or not 0 < len(result["error"]) <= 4096
            ):
                raise ValueError("Invalid plugin runtime error")
            raise RuntimeOpenError(result["error"])
        return result


class PanelRuntimePool:
    def __init__(self, home, entries):
        self.home = home
        self.entries = entries
        self.runtimes = {}

    @staticmethod
    def key(owner, project, plugin_id, panel):
        return (
            owner,
            str(project),
            plugin_id,
            panel["id"],
            str(panel["root"]),
            str(panel["executable"]),
            panel["interpreter"],
            panel["revision"],
        )

    def acquire(self, owner, project, plugin_id, panel):
        key = self.key(owner, project, plugin_id, panel)
        runtime = self.runtimes.get(key)
        if runtime is not None and runtime.process.poll() is not None:
            self.discard(runtime)
            runtime = None
        if runtime is None:
            ordinary = sum("runtime" not in entry for entry in self.entries.values())
            if len(self.runtimes) + ordinary >= 16:
                raise BrokerError.limit_exceeded(
                    "Close a session before starting another plugin runtime"
                )
            storage_id = hashlib.sha256(json.dumps(key).encode()).hexdigest()
            directory = self.home / "host-gui" / "runtimes" / storage_id
            runtime = PanelRuntime(
                launch_process(
                    self.home,
                    panel,
                    project,
                    directory,
                    reuse=True,
                )
            )
            self.runtimes[key] = runtime
        return runtime

    def discard(self, runtime):
        for key, current in list(self.runtimes.items()):
            if current is runtime:
                self.runtimes.pop(key)
        for token, entry in list(self.entries.items()):
            if entry.get("runtime") is runtime:
                self.entries.pop(token)
        runtime.stopped.set()
        stop_process(runtime.process)

    def open(self, runtime, token, context):
        try:
            return runtime.request(
                "open", instance_id=token, context=context, timeout=180
            )
        except RuntimeOpenError:
            raise
        except (OSError, ValueError):
            self.discard(runtime)
            raise

    def close_instance(self, token, entry):
        runtime = entry["runtime"]
        try:
            result = runtime.request("close", instance_id=token)
            if result.get("ok") is not True:
                raise ValueError("Invalid plugin runtime close response")
        except (OSError, ValueError):
            self.discard(runtime)

    def prune(self, owner, project, panels, *, plugin_id=None, check_instances=True):
        for key, runtime in list(self.runtimes.items()):
            if key[0] != owner or plugin_id is not None and key[2] != plugin_id:
                continue
            panel = panels.get((key[2], key[3]))
            if (
                panel is None
                or panel["reuse"] != "session"
                or key != self.key(owner, project, key[2], panel)
                or runtime.process.poll() is not None
            ):
                self.discard(runtime)
                continue
            if not check_instances or not runtime.ready.is_set():
                continue
            try:
                result = runtime.request("status")
                instances = result.get("instances")
                if (
                    not isinstance(instances, list)
                    or len(instances) > 16
                    or any(
                        not isinstance(instance, str) or len(instance) != 48
                        for instance in instances
                    )
                    or len(set(instances)) != len(instances)
                ):
                    raise ValueError("Invalid plugin runtime status response")
                for token, entry in list(self.entries.items()):
                    if entry.get("runtime") is runtime and token not in instances:
                        self.entries.pop(token)
            except (OSError, ValueError):
                self.discard(runtime)

    def close_owner(self, owner):
        for key, runtime in list(self.runtimes.items()):
            if key[0] == owner:
                self.discard(runtime)
