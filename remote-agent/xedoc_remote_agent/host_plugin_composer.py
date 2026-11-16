"""Bounded, explicit composer contributions from trusted installed plugins."""

import json
import os
import queue
import re
import signal
import shutil
import subprocess
import sys
import threading
import time

from .errors import BrokerError


def _text(value, limit):
    return (
        isinstance(value, str) and bool(value.strip()) and len(value) <= limit
        and not any(ord(character) < 32 for character in value)
    )


def declarations(root, values):
    if not isinstance(values, list) or len(values) > 16:
        raise ValueError("Invalid composer buttons")
    buttons = []
    for value in values:
        identity, title = value["id"], value["title"]
        entrypoint, interpreter = value["entrypoint"], value.get("interpreter")
        mode, icon = value.get("mode"), value.get("icon")
        if (
            not isinstance(identity, str) or not re.fullmatch(r"[A-Za-z0-9_.-]{1,80}", identity)
            or not _text(title, 120) or mode not in ("menu", "action")
            or icon is not None and not _text(icon, 4)
            or not isinstance(entrypoint, str) or not entrypoint.startswith("./")
            or interpreter not in (None, "python", "node")
            or any(button["buttonId"] == identity for button in buttons)
        ):
            raise ValueError("Invalid composer button")
        executable = (root / entrypoint).resolve()
        if not executable.is_relative_to(root) or not executable.is_file():
            raise ValueError("Composer entrypoint is outside the plugin")
        buttons.append({
            "buttonId": identity, "title": title, "icon": icon, "mode": mode,
            "executable": executable, "interpreter": interpreter,
        })
    return buttons


def _run(button, context, project):
    command = [str(button["executable"])]
    if button["interpreter"]:
        runtime = sys.executable if button["interpreter"] == "python" else shutil.which("node")
        if not runtime:
            raise BrokerError.unavailable("The plugin's Node.js runtime is unavailable")
        command.insert(0, runtime)
    process = None
    try:
        deadline = time.monotonic() + 10
        process = subprocess.Popen(
            command, cwd=project, stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            start_new_session=os.name != "nt", bufsize=0,
        )
        output = queue.Queue(maxsize=1)

        def read_output():
            data = bytearray()
            try:
                while len(data) <= 256 * 1024:
                    chunk = process.stdout.read(256 * 1024 + 1 - len(data))
                    if not chunk:
                        break
                    data.extend(chunk)
            except (OSError, ValueError):
                pass
            output.put(bytes(data))

        threading.Thread(
            target=read_output,
            daemon=True, name="plugin-composer-output",
        ).start()
        process.stdin.write((json.dumps(context) + "\n").encode())
        process.stdin.close()
        data = output.get(timeout=max(0, deadline - time.monotonic()))
        if len(data) > 256 * 1024:
            raise ValueError("Composer response exceeds 256 KiB")
        process.wait(timeout=max(0, deadline - time.monotonic()))
        if process.returncode:
            raise ValueError("Composer command failed")
        if not data.endswith(b"\n") or data.count(b"\n") != 1:
            raise ValueError("Composer command must return one JSON line")
        result = json.loads(data)
        if not isinstance(result, dict):
            raise ValueError("Composer response must be an object")
        return result
    except (queue.Empty, subprocess.TimeoutExpired) as error:
        raise BrokerError.unavailable("Composer command timed out after 10 seconds") from error
    except (OSError, ValueError, RecursionError) as error:
        raise BrokerError.unavailable(f"Invalid composer command response: {error}") from error
    finally:
        if process is not None:
            if os.name != "nt":
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            elif process.poll() is None:
                process.kill()
            process.wait()
            process.stdout.close()
            if not process.stdin.closed:
                process.stdin.close()


def _menu(result):
    count = 0

    def visit(items, depth):
        nonlocal count
        if not isinstance(items, list) or depth > 16:
            raise ValueError("Invalid composer menu depth or items")
        public = []
        for item in items:
            count += 1
            if count > 1000 or not isinstance(item, dict) or not _text(item.get("title"), 255):
                raise ValueError("Invalid composer menu item or count")
            node = {"title": item["title"]}
            if "children" in item:
                if "target" in item or not isinstance(item.get("expanded", False), bool):
                    raise ValueError("Invalid composer menu branch")
                node.update(
                    children=visit(item["children"], depth + 1),
                    expanded=item.get("expanded", False),
                )
            elif _text(item.get("target"), 4096):
                node["target"] = item["target"]
            else:
                raise ValueError("Invalid composer menu target")
            public.append(node)
        return public

    menu = {"items": visit(result.get("items"), 1)}
    if "emptyMessage" in result:
        if not _text(result["emptyMessage"], 255):
            raise ValueError("Invalid composer menu empty message")
        menu["emptyMessage"] = result["emptyMessage"]
    return menu


def invoke(panels, account, workspace, project_name, thread_id, plugin_id, panel_id, button_id, target):
    if any(not isinstance(value, str) for value in (plugin_id, panel_id, button_id)):
        raise BrokerError.invalid_request("Plugin, panel, and composer button IDs are required")
    with panels.lock:
        session = panels._session(account, workspace, project_name, thread_id)
        panel = panels.catalog(session["project"], plugin_id=plugin_id).get((plugin_id, panel_id))
        button = next(
            (item for item in panel["composerButtons"] if item["buttonId"] == button_id),
            None,
        ) if panel else None
        if button is None:
            raise BrokerError.not_found("The enabled plugin does not declare this composer button")
        project = session["project"]
    if target is not None:
        if button["mode"] != "menu":
            raise BrokerError.invalid_request("Only menu buttons accept a selected target")
    else:
        result = _run(button, {
            "protocol": "xedoc.plugin-composer/v1", "pluginId": plugin_id,
            "panelId": panel_id, "buttonId": button_id, "threadId": thread_id,
            "projectRoot": str(project),
        }, project)
        try:
            if button["mode"] == "menu":
                return _menu(result)
            if result and (set(result) != {"target"} or not _text(result["target"], 4096)):
                raise ValueError("Action must return an empty object or a target")
            target = result.get("target")
        except ValueError as error:
            raise BrokerError.unavailable(str(error)) from error
    if target is None:
        return {}
    return {"panel": panels.open(
        account, workspace, project_name, thread_id, plugin_id, panel_id, target,
    )}
