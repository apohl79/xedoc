"""Session-scoped browser automation requests for native Host GUI panels."""

from collections import deque
import json
import math
import re
import threading
import time
import uuid
from urllib.parse import urlsplit

from .errors import BrokerError
from .host_outside_projects import OTHER_WORKSPACE


_ACTIONS = {
    "open", "list", "navigate", "back", "forward", "reload", "snapshot",
    "evaluate", "click", "fill", "select", "scroll", "screenshot", "close",
    "frames", "inspect", "wait_for", "focus", "observe", "diagnostics",
    "pointer", "key", "type", "cdp", "cdp_events", "extensions",
    "downloads", "save_download", "set_presentation",
}
_MAX_PENDING = 32
_MAX_POLL_SESSIONS = 128
_LEGACY_POLL = object()
_MAX_RESULT_BYTES = 6 * 1024 * 1024
_PANEL_ID = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\Z",
    re.IGNORECASE,
)


class HostBrowser:
    def __init__(self, panels):
        self.panels = panels
        self.condition = threading.Condition(threading.RLock())
        self.pending = {}
        self.queues = {}
        self.poll_cursor = 0

    def command(self, account, body):
        thread_id, action, arguments, timeout = self._validate(body)
        # Commands use the local host-owner bearer; a connected GUI may use
        # another account, including an authenticated remote GUI.
        with self.panels.lock:
            candidates = [
                session for (_, thread), session in self.panels.sessions.items()
                if thread == thread_id
                and time.monotonic() - session.get("browserSeenAt", 0) <= 5
            ]
            if candidates:
                target = max(candidates, key=lambda session: session["browserSeenAt"])
                user = self.panels.auth.users.get(target["username"])
                if user is not None:
                    account = {"username": target["username"], **user}
        session = self._session(account, thread_id)
        with self.panels.lock:
            if time.monotonic() - session.get("browserSeenAt", 0) > 5:
                raise BrokerError.unavailable(
                    "Browser automation requires this session open in the native Host GUI"
                )
        request_id = str(uuid.uuid4())
        request = {
            "requestId": request_id,
            "threadId": thread_id,
            "username": account["username"],
            "workspaceId": session["workspaceId"],
            "projectName": session["projectName"],
            "action": action,
            "arguments": arguments,
            "timeout": timeout,
            "event": threading.Event(),
        }
        owner = (account["username"], thread_id)
        with self.condition:
            if len(self.pending) >= _MAX_PENDING:
                raise BrokerError.limit_exceeded("Too many browser requests are pending")
            self.pending[request_id] = request
            self.queues.setdefault(owner, deque()).append(request_id)
            self.condition.notify_all()

        if not request["event"].wait(timeout):
            with self.condition:
                self.pending.pop(request_id, None)
                queue = self.queues.get(owner)
                if queue is not None:
                    try:
                        queue.remove(request_id)
                    except ValueError:
                        pass
                    if not queue:
                        self.queues.pop(owner, None)
            raise BrokerError.unavailable(
                "Browser request timed out; keep its session open in the native Host GUI"
            )
        with self.condition:
            self.pending.pop(request_id, None)
        if request.get("error"):
            raise BrokerError.invalid_request(request["error"])
        return {"result": request.get("result")}

    def poll(self, account, workspace_id, project_name, thread_id, sessions=_LEGACY_POLL):
        registrations = [{
            "workspaceId": workspace_id,
            "projectName": project_name,
            "threadId": thread_id,
        }] if sessions is _LEGACY_POLL else sessions
        if not isinstance(registrations, list) or not 1 <= len(registrations) <= _MAX_POLL_SESSIONS:
            raise BrokerError.invalid_request("Browser poll requires between 1 and 128 sessions")
        with self.panels.lock:
            registered = {}
            for registration in registrations:
                if not isinstance(registration, dict) or any(
                    not isinstance(registration.get(key), str)
                    or not 0 < len(registration[key]) <= 1024
                    for key in ("workspaceId", "projectName", "threadId")
                ):
                    raise BrokerError.invalid_request("Browser poll session is invalid")
                if account["role"] != "admin" and registration["workspaceId"] == OTHER_WORKSPACE:
                    raise BrokerError.unauthorized("Workspace access denied")
                session = self._session(
                    account, registration["threadId"],
                    registration["workspaceId"], registration["projectName"],
                )
                registered[(account["username"], registration["threadId"])] = session
            seen_at = time.monotonic()
            for session in registered.values():
                session["browserSeenAt"] = seen_at
        owners = list(registered)
        deadline = time.monotonic() + 2
        with self.condition:
            while True:
                for offset in range(len(owners)):
                    index = (self.poll_cursor + offset) % len(owners)
                    owner = owners[index]
                    queue = self.queues.get(owner)
                    while queue:
                        request = self.pending.get(queue.popleft())
                        if request is not None:
                            self.poll_cursor = (index + 1) % len(owners)
                            request["delivered"] = True
                            return {
                                "command": {
                                    key: request[key]
                                    for key in (
                                        "requestId", "threadId", "workspaceId",
                                        "projectName", "action", "arguments", "timeout",
                                    )
                                }
                            }
                    self.queues.pop(owner, None)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return {"command": None}
                self.condition.wait(remaining)

    def complete(self, account, workspace_id, project_name, thread_id, body):
        session = self._session(account, thread_id, workspace_id, project_name)
        with self.panels.lock:
            session["browserSeenAt"] = time.monotonic()
        request_id = body.get("requestId")
        if not isinstance(request_id, str) or not _PANEL_ID.fullmatch(request_id):
            raise BrokerError.invalid_request("A valid browser request ID is required")
        result = body.get("result")
        error = body.get("error")
        if error is not None and (not isinstance(error, str) or len(error.encode()) > 2048):
            raise BrokerError.invalid_request("Browser error is invalid or too large")
        try:
            result_size = len(json.dumps(result, allow_nan=False).encode())
        except (TypeError, ValueError) as cause:
            raise BrokerError.invalid_request("Browser result is not valid JSON") from cause
        if result_size > _MAX_RESULT_BYTES:
            raise BrokerError.limit_exceeded("Browser result exceeds 6 MiB")

        owner = (account["username"], thread_id)
        with self.condition:
            request = self.pending.get(request_id)
            if request is None or (request["username"], request["threadId"]) != owner:
                raise BrokerError.not_found("Browser request is no longer pending")
            request["result"] = result
            request["error"] = error
            request["event"].set()
            self.condition.notify_all()
        return {"ok": True}

    def _session(self, account, thread_id, workspace_id=None, project_name=None):
        if not isinstance(thread_id, str) or not thread_id or len(thread_id) > 1024:
            raise BrokerError.invalid_request("A session thread ID is required")
        with self.panels.lock:
            session = self.panels.sessions.get((account["username"], thread_id))
            if session is None:
                raise BrokerError.unavailable(
                    "Open the target session in the native Host GUI first"
                )
            workspace_id = session["workspaceId"] if workspace_id is None else workspace_id
            project_name = session["projectName"] if project_name is None else project_name
            self.panels._session(account, workspace_id, project_name, thread_id)
            return session

    @staticmethod
    def _validate(body):
        thread_id = body.get("threadId")
        action = body.get("command")
        arguments = body.get("arguments", {})
        timeout = body.get("timeout", 30)
        if not isinstance(thread_id, str) or not thread_id or len(thread_id) > 1024:
            raise BrokerError.invalid_request("A session thread ID is required")
        if not isinstance(action, str) or action not in _ACTIONS:
            raise BrokerError.invalid_request("Unsupported browser action")
        if not isinstance(arguments, dict):
            raise BrokerError.invalid_request("Browser arguments must be a JSON object")
        if (
            isinstance(timeout, bool)
            or not isinstance(timeout, (int, float))
            or not math.isfinite(timeout)
            or not 1 <= timeout <= 120
        ):
            raise BrokerError.invalid_request("Browser timeout must be between 1 and 120 seconds")

        if action not in ("open", "list", "extensions"):
            panel_id = arguments.get("panelId")
            if not isinstance(panel_id, str) or not _PANEL_ID.fullmatch(panel_id):
                raise BrokerError.invalid_request("A valid website panelId is required")
        if action in ("open", "navigate"):
            url = arguments.get("url")
            if not isinstance(url, str) or len(url.encode()) > 16_384:
                raise BrokerError.invalid_request("A bounded website URL is required")
            parsed = urlsplit(url)
            if (
                parsed.scheme.lower() not in {"http", "https"}
                or not parsed.hostname
                or parsed.username is not None
                or parsed.password is not None
            ):
                raise BrokerError.invalid_request("Only HTTP and HTTPS website URLs are allowed")
        if action == "set_presentation" and "presentation" not in arguments:
            raise BrokerError.invalid_request("Browser presentation is required")
        if "presentation" in arguments and arguments["presentation"] not in (
            "foreground", "background",
        ):
            raise BrokerError.invalid_request("Browser presentation must be foreground or background")
        if action == "save_download":
            download_id = arguments.get("downloadId")
            if not isinstance(download_id, str) or not _PANEL_ID.fullmatch(download_id):
                raise BrokerError.invalid_request("A valid downloadId is required")
            offset = arguments.get("offset", 0)
            if type(offset) is not int or not 0 <= offset <= 32 * 1024 * 1024:
                raise BrokerError.invalid_request("Invalid download offset")
        if action == "evaluate":
            script = arguments.get("script")
            if not isinstance(script, str) or len(script.encode()) > 32 * 1024:
                raise BrokerError.invalid_request("Browser script is missing or exceeds 32 KiB")
        if action in ("cdp", "cdp_events"):
            method = arguments.get("method")
            if (action == "cdp" or method is not None) and (
                not isinstance(method, str) or not re.fullmatch(r"[A-Za-z]+\.[A-Za-z]+", method)
                or len(method) > 128
            ):
                raise BrokerError.invalid_request("A bounded DevTools method is required")
            if not isinstance(arguments.get("params", {}), dict):
                raise BrokerError.invalid_request("DevTools params must be an object")
            session_id = arguments.get("sessionId")
            if session_id is not None and (not isinstance(session_id, str) or not 0 < len(session_id) <= 128):
                raise BrokerError.invalid_request("Invalid DevTools session ID")
            result_path = arguments.get("resultPath", [])
            if not isinstance(result_path, list) or len(result_path) > 8 or any(
                not isinstance(key, str) or len(key) > 128 for key in result_path
            ):
                raise BrokerError.invalid_request("Invalid DevTools result path")
        frame_path = arguments.get("framePath", [])
        if (
            not isinstance(frame_path, list) or len(frame_path) > 8
            or any(not isinstance(item, str) or not 0 < len(item.encode()) <= 1024 for item in frame_path)
        ):
            raise BrokerError.invalid_request("framePath must contain at most 8 bounded CSS selectors")
        if action in ("click", "fill", "select", "inspect", "focus"):
            selector = arguments.get("selector")
            if not isinstance(selector, str) or not 0 < len(selector.encode()) <= 4096:
                raise BrokerError.invalid_request("A bounded CSS selector is required")
        if action in ("snapshot", "wait_for", "scroll") and "selector" in arguments:
            selector = arguments["selector"]
            if not isinstance(selector, str) or not 0 < len(selector.encode()) <= 4096:
                raise BrokerError.invalid_request("A bounded CSS selector is required")
        for name, minimum, maximum in (
            ("offset", 0, 2**53 - 1 if action in ("cdp", "save_download") else 10000),
            ("limit", 1, 4096 if action == "cdp" else 8 if action == "cdp_events" else 12),
            ("after", 0, 2**53 - 1), ("textLimit", 0, 3000),
            ("timeoutMs", 100, 15000), ("id", 1, 2**53 - 1),
        ):
            if name in arguments:
                value = arguments[name]
                if type(value) is not int or not minimum <= value <= maximum:
                    raise BrokerError.invalid_request(f"Browser {name} must be between {minimum} and {maximum}")
        for name in ("enabled", "captureBodies"):
            if name in arguments and not isinstance(arguments[name], bool):
                raise BrokerError.invalid_request(f"Browser {name} must be boolean")
        for name, choices in (
            ("state", ("attached", "visible", "hidden")),
            ("kind", ("console", "network", "resources")),
        ):
            if name in arguments and arguments[name] not in choices:
                raise BrokerError.invalid_request(f"Browser {name} is invalid")
        if "text" in arguments and (
            not isinstance(arguments["text"], str)
            or len(arguments["text"].encode()) > (4096 if action == "type" else 1024)
        ):
            raise BrokerError.invalid_request("Browser text is invalid or too large")
        if action == "type" and not isinstance(arguments.get("text"), str):
            raise BrokerError.invalid_request("Text is required")
        if action == "key":
            key = arguments.get("key")
            named = {
                "Enter", "Tab", "Escape", "Backspace", "Delete", "ArrowUp", "ArrowDown",
                "ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown",
            }
            if not isinstance(key, str) or not (
                key in named or len(key) == 1 and 32 <= ord(key) <= 126
            ):
                raise BrokerError.invalid_request("Unsupported native key")
            modifiers = arguments.get("modifiers", [])
            if not isinstance(modifiers, list) or len(modifiers) > 4 or any(
                item not in ("shift", "control", "alt", "meta") for item in modifiers
            ):
                raise BrokerError.invalid_request("Unsupported key modifiers")
        if action == "pointer":
            if arguments.get("event") not in ("move", "down", "up", "click"):
                raise BrokerError.invalid_request("Unsupported pointer event")
            for name in ("x", "y"):
                value = arguments.get(name)
                if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or not 0 <= value <= 100_000:
                    raise BrokerError.invalid_request("Pointer coordinates are invalid")
        if action == "fill":
            value = arguments.get("value")
            if not isinstance(value, str) or len(value.encode()) > 8192:
                raise BrokerError.invalid_request("Fill value is invalid or exceeds 8 KiB")
        if action == "select":
            value = arguments.get("value")
            if not isinstance(value, str) or len(value.encode()) > 4096:
                raise BrokerError.invalid_request("Select value is invalid or exceeds 4 KiB")
        if action == "scroll":
            selector = arguments.get("selector")
            if selector is not None and (
                not isinstance(selector, str) or len(selector.encode()) > 4096
            ):
                raise BrokerError.invalid_request("Scroll selector is invalid or too large")
            for key in ("x", "y"):
                value = arguments.get(key, 0)
                if (
                    isinstance(value, bool)
                    or not isinstance(value, (int, float))
                    or not math.isfinite(value)
                    or abs(value) > 100_000
                ):
                    raise BrokerError.invalid_request(f"Scroll offset {key} is invalid")
        return thread_id, action, arguments, float(timeout)
