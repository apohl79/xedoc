#!/usr/bin/env python3
"""Exercise the real host manager and Inline Discussion runtime, without a GUI.

PYTHONPATH=remote-agent python3 remote-agent/functional/plugin_runtime.py \
    /absolute/path/to/plugins/inline-discussion

Uses isolated temporary home/project state. Only the app-server catalog and
inference snapshot boundaries are fixtures; panel processes and HTTP are real.
Additional child processes inject malformed, mismatched, EOF, and stalled replies.
"""

import json
from pathlib import Path
import sys
import tempfile
import threading
import time
from urllib.error import URLError
from urllib.request import Request, urlopen

from xedoc_remote_agent.host_plugin_panels import HostPluginPanels
from xedoc_remote_agent.host_plugin_runtime import (
    PROTOCOL,
    PanelRuntime,
    RuntimeOpenError,
    launch_process,
)


def main():
    plugin_root = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="xedoc-warm-panel-") as directory:
        root = Path(directory).resolve()
        project, home = root / "project", root / "home"
        project.mkdir()
        home.mkdir()
        for name in ("first", "second"):
            (project / f"{name}.md").write_text(f"# {name}\n\n{name} paragraph.\n")
        rollout = root / "main.jsonl"
        rollout.write_text("")
        bootstrap_path = root / "bootstrap.toml"
        bootstrap_path.write_text(f'controller = "unix://{root}/absent.sock"\n')
        account = {"username": "fixture", "role": "admin", "workspaces": []}

        class Workspaces:
            _bootstrap_path = bootstrap_path
            enabled = True
            catalog_gate = None

            def _request(self, method, params):
                if method == "plugin/installed":
                    gate = self.catalog_gate
                    if gate is not None:
                        gate[0].set()
                        gate[1].wait(5)
                    return {
                        "marketplaces": [
                            {
                                "path": str(root),
                                "plugins": [
                                    {
                                        "id": "inline-discussion@fixture",
                                        "name": "inline-discussion",
                                        "installed": True,
                                        "enabled": self.enabled,
                                    }
                                ],
                            }
                        ]
                    }
                assert method == "plugin/read"
                return {"plugin": {"localPath": str(plugin_root)}}

            def _thread_for_project(self, thread_id, selected_project):
                assert selected_project == project
                return {"path": str(rollout)}

        class Auth:
            def account(self, cookie):
                assert cookie == "fixture-cookie"
                return account

        class Inference:
            effort = "medium"

            def inference(self, selected_account, thread_id):
                assert selected_account == account
                return {
                    "threadId": thread_id,
                    "model": "fixture",
                    "modelProvider": "fixture",
                    "reasoningEffort": self.effort,
                    "models": [
                        {
                            "providerId": "fixture",
                            "model": "fixture",
                            "defaultReasoningEffort": "medium",
                            "supportedReasoningEfforts": [
                                {"reasoningEffort": effort, "description": effort}
                                for effort in ("medium", "high")
                            ],
                        }
                    ],
                }

        workspaces, inference = Workspaces(), Inference()
        panels = HostPluginPanels(home, workspaces, Auth(), inference)

        def attach(thread="fixture"):
            start = time.monotonic()
            panels.attach(
                account, "fixture-cookie", "workspace", "project", thread, project
            )
            assert time.monotonic() - start < 1, "attach waited for runtime imports"
            deadline = time.monotonic() + 30
            while True:
                with panels.lock:
                    runtime = next(
                        (
                            runtime
                            for key, runtime in panels.runtime_pool.runtimes.items()
                            if key[0] == ("fixture", thread)
                        ),
                        None,
                    )
                if runtime is not None:
                    break
                assert time.monotonic() < deadline, "prewarm did not start"
                time.sleep(0.01)
            assert runtime.ready.wait(30), "runtime imports did not complete"
            assert runtime.startup_error is None, runtime.startup_error
            return runtime

        def open_document(name, thread="fixture"):
            start = time.monotonic()
            public = panels.open(
                account,
                "workspace",
                "project",
                thread,
                "inline-discussion@fixture",
                "discussion",
                str(project / f"{name}.md"),
            )
            return public, panels.entry(public["panelId"]), time.monotonic() - start

        def request(entry, path="api/bootstrap", payload=None):
            address = entry["address"].geturl()
            url = address.rstrip("/") + "/" + path
            headers = {"Origin": address.rstrip("/")}
            data = None
            if payload is not None:
                data = json.dumps(payload).encode()
                headers["Content-Type"] = "application/json"
            with urlopen(
                Request(url, data=data, headers=headers), timeout=5
            ) as response:
                return json.load(response)

        try:
            started, release = threading.Event(), threading.Event()
            workspaces.catalog_gate = (started, release)
            try:
                panels.attach(
                    account,
                    "fixture-cookie",
                    "workspace",
                    "project",
                    "warming",
                    project,
                )
                assert started.wait(5), "warmup did not reach catalog"
                start = time.monotonic()
                panels.attach(
                    account, "fixture-cookie", "workspace", "project", "other", project
                )
                panels.close("fixture")
                assert time.monotonic() - start < 1, "warmup blocked GUI attach/close"
            finally:
                workspaces.catalog_gate = None
                release.set()
            print("PASS stalled speculative catalog does not block GUI attach/close")

            runtime = attach()
            first, first_entry, first_seconds = open_document("first")
            inference.effort = "high"
            second, second_entry, second_seconds = open_document("second")
            assert first["panelId"] != second["panelId"]
            assert first_entry["runtime"] is second_entry["runtime"] is runtime
            assert first_entry["address"].port != second_entry["address"].port
            assert len(panels.runtime_pool.runtimes) == 1
            first_boot, second_boot = request(first_entry), request(second_entry)
            assert first_boot["documentPath"] == str(project / "first.md")
            assert second_boot["documentPath"] == str(project / "second.md")
            assert first_boot["defaultInferenceSettings"]["reasoningEffort"] == "medium"
            assert second_boot["defaultInferenceSettings"]["reasoningEffort"] == "high"
            request(
                first_entry,
                "api/threads",
                {
                    "anchor": {"blockId": first_boot["blockIds"][1]},
                    "message": "First document only",
                    "kind": "note",
                },
            )
            assert len(request(first_entry)["threads"]) == 1
            assert request(second_entry)["threads"] == []
            print(
                f"PASS prewarmed first/new document: {first_seconds:.3f}s/{second_seconds:.3f}s; "
                f"one PID {runtime.process.pid}, separate ports/documents/inference/notes"
            )

            panels.close_panel(
                account, "workspace", "project", "fixture", first["panelId"]
            )
            try:
                request(first_entry)
            except (URLError, ConnectionError):
                pass
            else:
                raise AssertionError("closed instance still serves HTTP")
            assert request(second_entry)["documentPath"] == str(project / "second.md")
            assert runtime.process.poll() is None
            first, first_entry, _ = open_document("first")
            assert first_entry["runtime"] is runtime
            assert len(request(first_entry)["threads"]) == 1
            print("PASS closing/reopening one instance preserves its sibling and notes")

            request(first_entry, "api/pause", {})
            for _ in range(50):
                snapshot = panels.snapshot(account, "workspace", "project", "fixture")
                if first["panelId"] not in {
                    item["panelId"] for item in snapshot["panels"]
                }:
                    break
                time.sleep(0.02)
            else:
                raise AssertionError("paused instance remains in host snapshot")
            assert request(second_entry)["documentPath"] == str(project / "second.md")
            print(
                "PASS Pause prunes only its instance; warm runtime and sibling survive"
            )

            panels.close_panel(
                account, "workspace", "project", "fixture", second["panelId"]
            )
            assert not panels.entries and runtime.process.poll() is None
            second, second_entry, _ = open_document("second")
            assert second_entry["runtime"] is runtime
            print("PASS closing the last instance retains the runtime for reuse")

            try:
                panels.runtime_pool.open(runtime, "f" * 48, {"threadId": "another"})
            except RuntimeOpenError:
                pass
            else:
                raise AssertionError("invalid instance context was accepted")
            assert request(second_entry)["documentPath"] == str(project / "second.md")
            print("PASS instance-open error does not invalidate sibling servers")

            other_runtime = attach("another")
            other, other_entry, _ = open_document("second", "another")
            assert other_runtime.process.pid != runtime.process.pid
            panels.close("fixture", "fixture")
            assert runtime.process.poll() is not None
            assert request(other_entry)["documentPath"] == str(project / "second.md")
            print("PASS thread-scoped processes and owner cleanup remain isolated")

            workspaces.enabled = False
            panels.catalogs.clear()
            panels.snapshot(account, "workspace", "project", "another")
            assert other_runtime.process.poll() is not None
            assert not panels.entries and not panels.runtime_pool.runtimes
            print("PASS disabling the plugin revokes active and idle runtime state")

            legacy_entrypoint = plugin_root / "runtime/bin/xedoc-panel"
            plugin_root = root / "legacy-plugin"
            (plugin_root / ".xedoc-plugin").mkdir(parents=True)
            (plugin_root / "panel.py").write_text(
                "import os, sys\n"
                f"os.execv(sys.executable, [sys.executable, {str(legacy_entrypoint)!r}])\n"
            )
            (plugin_root / ".xedoc-plugin/plugin.json").write_text(
                json.dumps(
                    {
                        "gui": {
                            "panels": [
                                {
                                    "id": "discussion",
                                    "entrypoint": "./panel.py",
                                    "interpreter": "python",
                                    "linkHandlers": [
                                        {
                                            "id": "markdown",
                                            "title": "Discuss",
                                            "extensions": [".md"],
                                            "schemes": [],
                                        }
                                    ],
                                }
                            ]
                        },
                    }
                )
            )
            workspaces.enabled = True
            panels.attach(
                account, "fixture-cookie", "workspace", "project", "legacy", project
            )
            legacy, legacy_entry, _ = open_document("first", "legacy")
            assert "runtime" not in legacy_entry
            assert request(legacy_entry)["documentPath"] == str(project / "first.md")
            request(legacy_entry, "api/pause", {})
            assert legacy_entry["process"].wait(timeout=5) == 0
            panels.snapshot(account, "workspace", "project", "legacy")
            assert legacy["panelId"] not in panels.entries
            print(
                "PASS non-opted-in real runtime retains one-shot protocol and Pause exit"
            )

            for mode in ("framing", "mismatch", "eof", "timeout"):
                worker = root / f"{mode}.py"
                worker.write_text(
                    "import json, sys, time\n"
                    f"protocol = {PROTOCOL!r}\n"
                    "print(json.dumps({'protocol': protocol, 'ready': True}), flush=True)\n"
                    "for line in sys.stdin:\n"
                    f"    mode = {mode!r}\n"
                    "    if mode == 'eof': sys.exit(0)\n"
                    "    if mode == 'timeout': time.sleep(60)\n"
                    "    if mode == 'framing': print('{', flush=True)\n"
                    "    if mode == 'mismatch':\n"
                    "        print(json.dumps({'protocol': protocol, 'instanceId': 'wrong'}), flush=True)\n"
                )
                failed_runtime = PanelRuntime(
                    launch_process(
                        home,
                        {"executable": worker, "interpreter": "python"},
                        project,
                        root / f"log-{mode}",
                        reuse=True,
                    )
                )
                pool = panels.runtime_pool
                pool.runtimes[(mode,)] = failed_runtime
                token = "a" * 48
                entry = {"runtime": failed_runtime, "process": failed_runtime.process}
                pool.entries[token] = entry
                pool.entries["b" * 48] = entry
                try:
                    start = time.monotonic()
                    if mode == "timeout":
                        pool.close_instance(token, entry)
                        assert time.monotonic() - start < 8
                    else:
                        try:
                            pool.open(failed_runtime, token, {})
                        except ValueError:
                            pass
                        else:
                            raise AssertionError(f"{mode} response was accepted")
                    assert failed_runtime.process.poll() is not None
                    assert not pool.entries and not pool.runtimes
                finally:
                    pool.discard(failed_runtime)
            print(
                "PASS malformed, mismatched, EOF, and timed-out channels revoke the runtime"
            )
        finally:
            panels.close()


if __name__ == "__main__":
    main()
