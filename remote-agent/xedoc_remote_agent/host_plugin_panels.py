"""Host-only plugin panels, bound to an authenticated session owner."""

import json
import hashlib
import os
from pathlib import Path
import queue
import re
import secrets
import threading
import time
from urllib.parse import unquote, urlsplit
from urllib.request import url2pathname

from .errors import BrokerError, ErrorCode
from .host_plugin_composer import declarations as composer_declarations
from .host_plugin_composer import invoke as invoke_composer
from .host_plugin_runtime import PanelRuntimePool, RuntimeOpenError, launch_process, stop_process
from .workspaces import load_bootstrap_descriptor


PROTOCOL = "xedoc.plugin-panel/v1"
_IDENTIFIER = re.compile(r"[A-Za-z0-9_.-]{1,80}")
_SOURCE_RANGE = re.compile(r":\d+(?::\d+)?(?:-\d+(?::\d+)?)?$")


class HostPluginPanels:
    def __init__(self, home, workspaces, auth, session_guis):
        self.home = home
        self.workspaces = workspaces
        self.auth = auth
        self.session_guis = session_guis
        self.lock = threading.RLock()
        self.sessions = {}
        self.entries = {}
        self.runtime_pool = PanelRuntimePool(home, self.entries)
        self.catalogs = {}
        self.broker = None

    def attach(self, account, cookie, workspace, project_name, thread_id, project):
        with self.lock:
            owner = (account["username"], thread_id)
            previous = self.sessions.get(owner)
            if previous and previous["project"] != Path(project).resolve():
                self.close(*owner)
            self.sessions[owner] = {
                "username": account["username"],
                "workspaceId": workspace,
                "projectName": project_name,
                "threadId": thread_id,
                "project": Path(project).resolve(),
                "cookie": cookie,
            }
            threading.Thread(
                target=self._prewarm, args=(owner, self.sessions[owner]),
                daemon=True, name="plugin-panel-prewarm",
            ).start()

    def _prewarm(self, owner, session):
        try:
            project = session["project"]
            panels = self.catalog(project, fresh=True)
            with self.lock:
                if self.sessions.get(owner) is not session:
                    return
                self.runtime_pool.prune(owner, project, panels, check_instances=False)
                for (plugin_id, _), panel in panels.items():
                    if panel["reuse"] == "session":
                        self.runtime_pool.acquire(owner, project, plugin_id, panel)
        except (OSError, ValueError, RuntimeError):
            pass

    def _session(self, account, workspace, project_name, thread_id):
        session = self.sessions.get((account["username"], thread_id))
        if (
            session is None
            or (session["workspaceId"], session["projectName"])
            != (workspace, project_name)
            or account["role"] != "admin" and workspace not in account["workspaces"]
        ):
            raise BrokerError.not_found("Open the session in the host GUI first")
        return session

    def catalog(self, project, *, plugin_id=None, fresh=False):
        key = str(project)
        cached = self.catalogs.get(key)
        if plugin_id is None and not fresh and cached and time.monotonic() - cached[0] < 30:
            return cached[1]
        result = self.workspaces._request("plugin/installed", {"cwds": [key]})
        panels = {}
        for marketplace in result.get("marketplaces", [])[:64]:
            marketplace_path = marketplace.get("path")
            if not marketplace_path:
                continue
            for summary in marketplace.get("plugins", [])[:128]:
                if not summary.get("installed") or not summary.get("enabled"):
                    continue
                if plugin_id is not None and summary["id"] != plugin_id:
                    continue
                detail = self.workspaces._request(
                    "plugin/read",
                    {
                        "marketplacePath": marketplace_path,
                        "pluginName": summary["name"],
                    },
                )["plugin"]
                root_path = detail.get("localPath")
                if not root_path:
                    continue
                root = Path(root_path).resolve()
                manifest = root / ".xedoc-plugin" / "plugin.json"
                if not manifest.is_file() or manifest.stat().st_size > 256 * 1024:
                    continue
                try:
                    manifest_stat = manifest.stat()
                    contents = json.loads(manifest.read_text())
                    declarations = contents.get("gui", {})
                    for declaration in declarations.get("panels", [])[:16]:
                        panel = self._declaration(summary["id"], root, declaration)
                        panel["pluginName"] = summary["name"]
                        panel["revision"] = (manifest_stat.st_mtime_ns, manifest_stat.st_size)
                        panels[(summary["id"], panel["id"])] = panel
                except (OSError, ValueError, TypeError, KeyError):
                    continue
        if plugin_id is None:
            self.catalogs[key] = (time.monotonic(), panels)
            if len(self.catalogs) > 64:
                self.catalogs.pop(next(iter(self.catalogs)))
        elif cached:
            updated = {
                identity: panel for identity, panel in cached[1].items()
                if identity[0] != plugin_id
            }
            self.catalogs[key] = (cached[0], {**updated, **panels})
        return panels

    @staticmethod
    def _declaration(plugin_id, root, declaration):
        panel_id, title = declaration["id"], declaration.get("title")
        entrypoint = declaration["entrypoint"]
        if (
            not isinstance(panel_id, str)
            or not _IDENTIFIER.fullmatch(panel_id)
            or title is not None and (
                not isinstance(title, str) or not 0 < len(title) <= 120
            )
            or not isinstance(entrypoint, str)
            or not entrypoint.startswith("./")
        ):
            raise ValueError("Invalid panel declaration")
        executable = (root / entrypoint).resolve()
        if not executable.is_relative_to(root) or not executable.is_file():
            raise ValueError("Panel entrypoint is outside the plugin")
        interpreter = declaration.get("interpreter")
        if interpreter not in (None, "python", "node"):
            raise ValueError("Unsupported panel interpreter")
        reuse = declaration.get("reuse")
        if reuse not in (None, "session"):
            raise ValueError("Unsupported panel runtime reuse")
        handlers = []
        for handler in declaration.get("linkHandlers", [])[:16]:
            handler_id, label = handler["id"], handler["title"]
            extensions, schemes = handler.get("extensions", []), handler.get("schemes", [])
            if (
                not isinstance(handler_id, str)
                or not _IDENTIFIER.fullmatch(handler_id)
                or not isinstance(label, str)
                or not 0 < len(label) <= 120
                or not isinstance(extensions, list)
                or not isinstance(schemes, list)
                or len(extensions) + len(schemes) > 32
                or any(not isinstance(e, str) or not re.fullmatch(r"\.[a-z0-9]{1,16}", e) for e in extensions)
                or any(not isinstance(s, str) or not re.fullmatch(r"[a-z][a-z0-9+.-]{0,31}", s)
                       or s in {"javascript", "data", "vbscript", "file", "blob"} for s in schemes)
            ):
                raise ValueError("Invalid link handler")
            handlers.append({
                "pluginId": plugin_id, "panelId": panel_id,
                "handlerId": handler_id, "title": label,
                "extensions": extensions, "schemes": schemes,
            })
        return {
            "id": panel_id, "title": title, "executable": executable,
            "interpreter": interpreter, "root": root, "handlers": handlers, "reuse": reuse,
            "composerButtons": composer_declarations(root, declaration.get("composerButtons", [])),
        }

    def snapshot(self, account, workspace, project_name, thread_id):
        with self.lock:
            session = self._session(account, workspace, project_name, thread_id)
            panels = self.catalog(session["project"])
            owner = (account["username"], thread_id)
            self.runtime_pool.prune(owner, session["project"], panels)
            for token, entry in list(self.entries.items()):
                if entry["owner"] == owner and (
                    entry["process"].poll() is not None
                    or (entry["pluginId"], entry["manifestPanelId"]) not in panels
                ):
                    self._close_entry(token)
            return {
                "actions": [action for panel in panels.values() for action in panel["handlers"]],
                "composerButtons": [
                    {"pluginId": plugin_id, "panelId": panel_id,
                     **{key: button[key] for key in ("buttonId", "title", "icon", "mode")}}
                    for (plugin_id, panel_id), panel in panels.items()
                    for button in panel["composerButtons"]
                ],
                "panels": [self._public(token, entry) for token, entry in self.entries.items()
                           if entry["owner"] == owner],
            }

    def composer(self, account, workspace, project_name, thread_id, plugin_id, panel_id, button_id, target=None):
        return invoke_composer(
            self, account, workspace, project_name, thread_id, plugin_id, panel_id, button_id, target,
        )

    def actions(self, account, workspace, project_name, thread_id, target):
        with self.lock:
            session = self._session(account, workspace, project_name, thread_id)
            panels = self.catalog(session["project"])
            actions = []
            for panel in panels.values():
                for handler in panel["handlers"]:
                    try:
                        self._target(
                            target, panel, session["project"], account, handler["handlerId"]
                        )
                    except BrokerError as error:
                        if error.code not in {
                            ErrorCode.INVALID_REQUEST, ErrorCode.NOT_FOUND, ErrorCode.UNAUTHORIZED
                        }:
                            raise
                    else:
                        actions.append(handler)
            return {"actions": actions}

    @staticmethod
    def _target(raw, panel, project, account, handler_id):
        if not isinstance(raw, str) or not 0 < len(raw) <= 4096 or any(ord(c) < 32 for c in raw):
            raise BrokerError.invalid_request("A bounded file path or URL is required")
        path_reference = raw.split("#", 1)[0].split("?", 1)[0]
        without_range = _SOURCE_RANGE.sub("", path_reference)
        file_range = without_range != path_reference and any(
            without_range.lower().endswith(extension)
            for handler in panel["handlers"] for extension in handler["extensions"]
        )
        parsed = urlsplit(without_range if file_range else raw)
        scheme = parsed.scheme.lower()
        if parsed.netloc and (scheme == "" or scheme == "file" and parsed.netloc != "localhost"):
            raise BrokerError.invalid_request("Only local file URLs are supported")
        local = scheme in ("", "file") or bool(re.match(r"^[A-Za-z]:[\\/]", raw))
        if local:
            path = Path(_SOURCE_RANGE.sub("", (
                url2pathname(parsed.path) if scheme == "file"
                else unquote(parsed.path if scheme == "" else path_reference)
            )))
            target_path = (project / path).resolve()
            if not target_path.is_file():
                raise BrokerError.not_found("The linked file does not exist")
            if account["role"] != "admin" and not target_path.is_relative_to(project):
                raise BrokerError.unauthorized("The linked file is outside this project")
            target, extension = str(target_path), target_path.suffix.lower()
        else:
            target, extension = raw, ""
        handlers = [h for h in panel["handlers"] if handler_id is None or h["handlerId"] == handler_id]
        if not any(extension in h["extensions"] if local else scheme in h["schemes"] for h in handlers):
            raise BrokerError.invalid_request("No declared handler accepts this target")
        return target

    @staticmethod
    def _public(token, entry):
        prefix = f"/plugin-ui/{token}/"
        return {
            "panelId": token, "pluginId": entry["pluginId"],
            "title": entry["title"], "target": entry["target"],
            "url": f"{prefix}?xedocBase={prefix}",
        }

    def open(self, account, workspace, project_name, thread_id, plugin_id, panel_id, target, handler_id=None):
        with self.lock:
            session = self._session(account, workspace, project_name, thread_id)
            panels = self.catalog(session["project"], plugin_id=plugin_id)
            panel = panels.get((plugin_id, panel_id))
            if panel is None:
                raise BrokerError.not_found("The enabled plugin does not declare this panel")
            target = self._target(target, panel, session["project"], account, handler_id)
            owner = (account["username"], thread_id)
            self.runtime_pool.prune(owner, session["project"], panels, plugin_id=plugin_id)
            for token, entry in self.entries.items():
                if (entry["owner"], entry["pluginId"], entry["manifestPanelId"], entry["target"]) == (
                    owner, plugin_id, panel_id, target
                ) and entry["process"].poll() is None:
                    return self._public(token, entry)
            if len(self.entries) >= 16:
                raise BrokerError.limit_exceeded("Close a plugin panel before opening another")
            if panel["reuse"] is None and (
                len(self.runtime_pool.runtimes)
                + sum("runtime" not in entry for entry in self.entries.values()) >= 16
            ):
                raise BrokerError.limit_exceeded("Close a session before starting another plugin runtime")
            thread = self.workspaces._thread_for_project(thread_id, session["project"])
            inference = self.session_guis.inference(account, thread_id)
            descriptor = load_bootstrap_descriptor(self.workspaces._bootstrap_path)
            token = secrets.token_hex(24)
            storage_id = hashlib.sha256(json.dumps(
                [*owner, plugin_id, panel_id, target]
            ).encode()).hexdigest()
            directory = self.home / "host-gui" / "panels" / storage_id
            directory.mkdir(parents=True, exist_ok=True, mode=0o700)
            os.chmod(directory, 0o700)
            context = {
                "protocol": PROTOCOL, "pluginId": plugin_id, "panelId": panel_id,
                "threadId": thread_id, "projectRoot": str(session["project"]),
                "target": target, "home": str(self.home), "sessionDir": str(directory),
                "sessionSocket": str(descriptor.socket_path),
                "mainJsonlPath": thread.get("path"),
                "inference": inference,
            }
            process = None
            runtime = None
            try:
                if panel["reuse"] == "session":
                    runtime = self.runtime_pool.acquire(
                        owner, session["project"], plugin_id, panel,
                    )
                    process = runtime.process
                    result = self.runtime_pool.open(runtime, token, context)
                else:
                    process = launch_process(self.home, panel, session["project"], directory)
                    process.stdin.write((json.dumps(context, ensure_ascii=False) + "\n").encode())
                    process.stdin.close()
                    ready = queue.Queue(maxsize=1)
                    threading.Thread(
                        target=lambda: ready.put(process.stdout.readline(65537)),
                        daemon=True, name="plugin-panel-ready",
                    ).start()
                    try:
                        line = ready.get(timeout=180)
                    except queue.Empty as error:
                        raise ValueError("Plugin panel startup timed out") from error
                    result = json.loads(line)
                title = result.get("title")
                if title is not None and (
                    not isinstance(title, str) or not 0 < len(title.strip()) <= 255
                ):
                    raise ValueError("Invalid plugin panel title")
                address = urlsplit(result["url"])
                if (
                    address.scheme != "http" or address.hostname != "127.0.0.1"
                    or not address.port or address.username or address.password
                    or address.path not in ("", "/") or address.query or address.fragment
                ):
                    raise ValueError("Invalid plugin loopback address")
                entry = {
                    "owner": owner, "pluginId": plugin_id, "manifestPanelId": panel_id,
                    "title": title.strip() if title is not None else panel["pluginName"],
                    "target": target, "address": address,
                    "process": process,
                }
                if runtime is not None:
                    entry["runtime"] = runtime
                self.entries[token] = entry
                return self._public(token, entry)
            except (OSError, ValueError, KeyError, TypeError) as error:
                if runtime is not None and not isinstance(error, RuntimeOpenError):
                    self.runtime_pool.discard(runtime)
                elif process is not None and runtime is None:
                    self._stop(process)
                raise BrokerError.unavailable(
                    "Plugin panel failed to start; see host-gui runtime.log"
                ) from error

    def entry(self, token):
        with self.lock:
            entry = self.entries.get(token)
            session = self.sessions.get(entry["owner"]) if entry else None
            if session is None:
                raise BrokerError.not_found()
            account = self.auth.account(session["cookie"])
            self._session(account, session["workspaceId"], session["projectName"], session["threadId"])
            if entry["process"].poll() is not None:
                raise BrokerError.not_found("The plugin panel has stopped")
            return entry

    def close_panel(self, account, workspace, project_name, thread_id, token):
        with self.lock:
            self._session(account, workspace, project_name, thread_id)
            entry = self.entries.get(token)
            if entry is None or entry["owner"] != (account["username"], thread_id):
                raise BrokerError.not_found()
            self._close_entry(token)

    @staticmethod
    def _stop(process):
        stop_process(process)

    def _close_entry(self, token):
        entry = self.entries.pop(token)
        if "runtime" in entry:
            self.runtime_pool.close_instance(token, entry)
        else:
            self._stop(entry["process"])

    def close(self, username=None, thread_id=None, *, workspace_id=None):
        with self.lock:
            for owner, session in list(self.sessions.items()):
                if username is not None and owner[0] != username:
                    continue
                if thread_id is not None and owner[1] != thread_id:
                    continue
                if workspace_id is not None and session["workspaceId"] != workspace_id:
                    continue
                self.runtime_pool.close_owner(owner)
                for token, entry in list(self.entries.items()):
                    if entry["owner"] == owner:
                        self._close_entry(token)
                self.sessions.pop(owner)

    def open_local(self, request):
        if request.get("protocol") != PROTOCOL:
            raise BrokerError.invalid_request("Unsupported plugin panel protocol")
        thread_id = request.get("threadId")
        with self.lock:
            sessions = [s for s in self.sessions.values() if s["threadId"] == thread_id]
            if not sessions:
                raise BrokerError.unavailable("No active host GUI session")
            if len(sessions) != 1:
                raise BrokerError.conflict("Select a unique host GUI session owner")
            session = sessions[0]
            account = self.auth.account(session["cookie"])
            return self.open(
                account, session["workspaceId"], session["projectName"], thread_id,
                request.get("pluginId"), request.get("panelId"), request.get("target"),
            )
