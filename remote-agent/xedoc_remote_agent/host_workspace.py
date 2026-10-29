"""Host-owner workspace management over the local controller connection."""

import json
import os
import re
import subprocess
import tempfile
import threading
import time
from pathlib import Path
from typing import Any, Callable, Mapping
from urllib.parse import unquote, urlsplit

from .controller import _LiveTurnItems, _default_client_factory
from .errors import BrokerError, ErrorCode, map_controller_error
from .host_outside_projects import OTHER_WORKSPACE, outside_directory, outside_projects
from .work_admission import WorkAdmissionGate
from .workspaces import _validate_workspace_id, load_bootstrap_descriptor


_SESSION_ID_LIMIT = 128
_SESSION_STATE_LIMIT = 1024 * 1024
_SESSION_HISTORY_LIMIT = 10
_SESSION_PAGE_SIZE = 100
_WEB_TURN_LIMIT = 50
_WEB_ITEM_LIMIT = 40
_WEB_TEXT_LIMIT = 16_384
_WEB_TRANSCRIPT_LIMIT = 900_000
_PROJECT_CLONE_TIMEOUT = 600
_PROJECT_CLONE_LIMIT = 2
_PROJECT_CLONE_TEMP_PREFIX = ".xedoc-host-clone-"
_THREAD_SOURCES = [
    "cli",
    "vscode",
    "exec",
    "appServer",
    "subAgent",
    "subAgentReview",
    "subAgentCompact",
    "subAgentThreadSpawn",
    "subAgentOther",
    "unknown",
]


def _valid_project_name(name: Any) -> bool:
    return (
        isinstance(name, str)
        and bool(name.strip())
        and len(name) <= 255
        and name not in {".", ".."}
        and "/" not in name
        and "\\" not in name
        and "\0" not in name
    )


def _public_https_repository_name(remote_url: Any) -> str:
    if (
        not isinstance(remote_url, str)
        or not remote_url
        or len(remote_url) > 2048
        or remote_url != remote_url.strip()
        or any(character.isspace() or ord(character) < 32 for character in remote_url)
    ):
        raise BrokerError.invalid_request("A public HTTPS repository URL is required")
    try:
        parsed = urlsplit(remote_url)
        hostname = parsed.hostname
        port = parsed.port
    except ValueError as error:
        raise BrokerError.invalid_request(
            "A public HTTPS repository URL is required"
        ) from error
    if (
        parsed.scheme != "https"
        or not hostname
        or port == 0
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
    ):
        raise BrokerError.invalid_request("A public HTTPS repository URL is required")
    repository_path = parsed.path.rstrip("/")
    if not repository_path:
        raise BrokerError.invalid_request("A repository path is required")
    name = unquote(repository_path.rsplit("/", 1)[-1])
    if name.casefold().endswith(".git"):
        name = name[:-4]
    if not _valid_project_name(name):
        raise BrokerError.invalid_request("Repository URL has no valid project name")
    return name


def _github_repository(remote: Any) -> tuple[str, str]:
    if (
        not isinstance(remote, str)
        or not remote
        or len(remote) > 2048
        or remote != remote.strip()
        or any(character.isspace() or ord(character) < 32 for character in remote)
    ):
        raise BrokerError.invalid_request(
            "A GitHub repository or HTTPS URL is required"
        )
    if "://" in remote:
        try:
            parsed = urlsplit(remote)
            port = parsed.port
        except ValueError as error:
            raise BrokerError.invalid_request(
                "A GitHub repository or HTTPS URL is required"
            ) from error
        if (
            parsed.scheme != "https"
            or parsed.hostname != "github.com"
            or port is not None
            or parsed.username is not None
            or parsed.password is not None
            or parsed.query
            or parsed.fragment
        ):
            raise BrokerError.invalid_request(
                "Only github.com HTTPS repository URLs are supported"
            )
        parts = parsed.path.strip("/").split("/")
    else:
        parts = remote.split("/")
    if len(parts) != 2:
        raise BrokerError.invalid_request(
            "Use a GitHub repository URL or owner/repository"
        )
    owner, repository = parts
    if repository.casefold().endswith(".git"):
        repository = repository[:-4]
    if any(
        not re.fullmatch(r"[A-Za-z0-9_.-]{1,100}", part) or part in {".", ".."}
        for part in (owner, repository)
    ):
        raise BrokerError.invalid_request("GitHub repository name is invalid")
    return f"https://github.com/{owner}/{repository}", repository


class HostWorkspaces:
    def __init__(
        self,
        home: Path,
        *,
        bootstrap_path: Path | None = None,
        on_config_changed: Callable[[], None] | None = None,
        work_admission: WorkAdmissionGate | None = None,
    ) -> None:
        self.home = home
        self._bootstrap_path = bootstrap_path or (
            home / "remote-agent" / "bootstrap.toml"
        )
        self._on_config_changed = on_config_changed
        self._work_admission = work_admission or WorkAdmissionGate()
        self._session_state_path = home / "host-gui" / "closed-sessions.json"
        self._closed_sessions = self._read_session_state()
        self._client = None
        self._activity_client = None
        self._activity_lock = threading.Lock()
        self._project_clone_slots = threading.BoundedSemaphore(_PROJECT_CLONE_LIMIT)
        self._closed = False
        self._live_items = _LiveTurnItems()
        self._active_turns: dict[str, str] = {}
        self._lock = threading.RLock()
        self._restart_required = False

    def _read_session_state(self) -> dict[str, list[str]]:
        if not self._session_state_path.exists():
            return {}
        try:
            if self._session_state_path.stat().st_size > _SESSION_STATE_LIMIT:
                raise ValueError("Session state exceeds size limit")
            value = json.loads(self._session_state_path.read_text())
            if not isinstance(value, dict) or len(value) > 256:
                raise ValueError("Invalid session state")
            result = {}
            for username, thread_ids in value.items():
                if (
                    not isinstance(username, str)
                    or len(username) > 64
                    or not isinstance(thread_ids, list)
                    or len(thread_ids) > 4096
                    or any(
                        not isinstance(thread_id, str)
                        or len(thread_id) > _SESSION_ID_LIMIT
                        for thread_id in thread_ids
                    )
                ):
                    raise ValueError("Invalid session state")
                result[username] = list(dict.fromkeys(thread_ids))
            return result
        except (OSError, json.JSONDecodeError) as error:
            raise ValueError("Cannot read host GUI session state") from error

    def _persist_session_state(self) -> None:
        directory = self._session_state_path.parent
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(dir=directory)
        try:
            with os.fdopen(descriptor, "w") as stream:
                json.dump(self._closed_sessions, stream)
                stream.flush()
                os.fsync(stream.fileno())
            if os.path.getsize(temporary) > _SESSION_STATE_LIMIT:
                raise BrokerError.limit_exceeded("Host GUI session state is full")
            os.replace(temporary, self._session_state_path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    def _remember_closed(self, username: str, thread_id: str, closed: bool) -> None:
        previous = self._closed_sessions.get(username, [])
        updated = [value for value in previous if value != thread_id]
        if closed:
            updated.append(thread_id)
            updated = updated[-4096:]
        old_value = self._closed_sessions.get(username)
        if updated:
            self._closed_sessions[username] = updated
        else:
            self._closed_sessions.pop(username, None)
        try:
            self._persist_session_state()
        except (OSError, BrokerError):
            if old_value is None:
                self._closed_sessions.pop(username, None)
            else:
                self._closed_sessions[username] = old_value
            raise

    def _new_client(self, *, observe_notifications: bool) -> Any:
        descriptor = load_bootstrap_descriptor(self._bootstrap_path)
        client = _default_client_factory(descriptor.socket_path, 30.0)
        try:
            client.initialize("xedoc-host-gui", "Xedoc Host GUI", "0.1.0")
            if observe_notifications:
                set_handler = getattr(client, "set_notification_handler", None)
                if callable(set_handler):
                    set_handler(self._observe_notification)
            client.set_server_request_handler(lambda message: None)
            return client
        except Exception:
            client.close()
            raise

    def _request(self, method: str, params: dict) -> dict:
        with self._lock:
            try:
                if self._closed:
                    raise BrokerError.unavailable()
                if self._client is None:
                    self._client = self._new_client(observe_notifications=True)
                return self._client.request(method, params)
            except Exception as error:
                if self._client is not None:
                    self._client.close()
                    self._client = None
                raise map_controller_error(error) from error

    def _activity_request(self, method: str, params: dict) -> dict:
        with self._activity_lock:
            try:
                if self._closed:
                    raise BrokerError.unavailable()
                if self._activity_client is None:
                    self._activity_client = self._new_client(
                        observe_notifications=False
                    )
                return self._activity_client.request(method, params)
            except Exception as error:
                if self._activity_client is not None:
                    self._activity_client.close()
                    self._activity_client = None
                raise map_controller_error(error) from error

    def _config(self) -> tuple[dict, dict]:
        response = self._request("config/read", {"includeLayers": True})
        remote = response.get("config", {}).get("remote_agent") or {}
        if not remote:
            raise BrokerError.invalid_request(
                "Configure the host with xedoc-session remote-agent-configure first"
            )
        roots = remote.get("workspaces", {})
        if not isinstance(roots, dict) or len(roots) > 128:
            raise BrokerError.invalid_request("Invalid host workspace configuration")
        for workspace_id, root in roots.items():
            _validate_workspace_id(workspace_id)
            if not isinstance(root, str) or not Path(root).is_absolute():
                raise BrokerError.invalid_request("Invalid host workspace root")
        layers = response.get("layers") or []
        user = next(
            (
                layer
                for layer in layers
                if layer.get("name", {}).get("type") == "user"
                and layer["name"].get("profile") is None
            ),
            None,
        )
        if not user or not isinstance(user.get("version"), str):
            raise BrokerError.unavailable("Host user config layer is unavailable")
        return dict(roots), user

    def snapshot(self) -> dict:
        with self._lock:
            roots, _ = self._config()
            return {
                "workspaces": [
                    {"workspaceId": key, "root": root}
                    for key, root in sorted(roots.items())
                ],
                "restartRequired": self._restart_required,
            }

    def browse(self, path: str) -> dict:
        if (
            not isinstance(path, str)
            or len(path) > 16384
            or not Path(path).is_absolute()
        ):
            raise BrokerError.invalid_request("An absolute directory path is required")
        try:
            directory = Path(path).resolve(strict=True)
            if not directory.is_dir():
                raise BrokerError.invalid_request("Path is not a directory")
            entries = []
            with os.scandir(directory) as children:
                for child in children:
                    if child.is_dir(
                        follow_symlinks=False
                    ) and not child.name.startswith(_PROJECT_CLONE_TEMP_PREFIX):
                        entries.append(
                            {"name": child.name, "path": str(directory / child.name)}
                        )
                        if len(entries) > 500:
                            raise BrokerError.limit_exceeded(
                                "Directory has more than 500 folders"
                            )
            return {
                "path": str(directory),
                "parent": str(directory.parent)
                if directory != directory.parent
                else None,
                "entries": sorted(entries, key=lambda item: item["name"].casefold()),
            }
        except OSError as error:
            raise BrokerError.invalid_request("Directory cannot be read") from error

    def _write(self, roots: dict, layer: dict) -> None:
        result = self._request(
            "config/value/write",
            {
                "keyPath": "remote_agent.workspaces",
                "value": roots,
                "mergeStrategy": "replace",
                "filePath": layer["name"]["file"],
                "expectedVersion": layer["version"],
            },
        )
        if result.get("status") != "ok":
            raise BrokerError.conflict(
                "Workspace config is overridden by another host layer"
            )
        self._restart_required = True
        if self._on_config_changed is not None:
            self._on_config_changed()

    def has_active_turns(self) -> bool:
        with self._lock:
            active_turns = tuple(self._active_turns.items())
        for thread_id, turn_id in active_turns:
            try:
                response = self._request(
                    "thread/turns/list",
                    {
                        "threadId": thread_id,
                        "limit": 1,
                        "sortDirection": "desc",
                        "itemsView": "notLoaded",
                    },
                )
            except BrokerError as error:
                if error.code is not ErrorCode.NOT_FOUND:
                    return True
                still_active = False
            except Exception:
                return True
            else:
                turns = response.get("data", [])
                still_active = isinstance(turns, list) and any(
                    isinstance(turn, dict)
                    and turn.get("id") == turn_id
                    and turn.get("status") in {"inProgress", "active"}
                    for turn in turns
                )
            with self._lock:
                if not still_active and self._active_turns.get(thread_id) == turn_id:
                    self._active_turns.pop(thread_id, None)
        with self._lock:
            return bool(self._active_turns)

    def _observe_notification(self, message: Mapping[str, Any]) -> None:
        self._live_items.observe(message)
        params = message.get("params")
        if not isinstance(params, dict):
            return
        thread_id = params.get("threadId")
        turn = params.get("turn")
        turn_id = turn.get("id") if isinstance(turn, dict) else None
        if not isinstance(thread_id, str) or not isinstance(turn_id, str):
            return
        with self._lock:
            if message.get("method") == "turn/started":
                self._active_turns[thread_id] = turn_id
            elif (
                message.get("method") == "turn/completed"
                and self._active_turns.get(thread_id) == turn_id
            ):
                self._active_turns.pop(thread_id, None)

    def add(self, workspace_id: str, root: str, create: bool) -> None:
        _validate_workspace_id(workspace_id)
        if not isinstance(create, bool):
            raise BrokerError.invalid_request("Create must be a boolean")
        if (
            not isinstance(root, str)
            or len(root) > 16384
            or not Path(root).is_absolute()
        ):
            raise BrokerError.invalid_request("An absolute workspace root is required")
        with self._lock:
            roots, layer = self._config()
            if workspace_id in roots or len(roots) >= 128:
                raise BrokerError.conflict(
                    "Workspace already exists or registry is full"
                )
            try:
                candidate = Path(root).resolve()
                if create:
                    candidate.mkdir(parents=True, exist_ok=True)
                candidate = candidate.resolve(strict=True)
                if not candidate.is_dir() or not os.access(
                    candidate, os.R_OK | os.X_OK
                ):
                    raise OSError
            except (OSError, RuntimeError, ValueError) as error:
                raise BrokerError.invalid_request(
                    "Workspace root is unavailable"
                ) from error
            if any(candidate == Path(value).resolve() for value in roots.values()):
                raise BrokerError.conflict("Workspace root is already registered")
            roots[workspace_id] = str(candidate)
            self._write(roots, layer)

    def remove(self, workspace_id: str) -> None:
        _validate_workspace_id(workspace_id)
        with self._lock:
            roots, layer = self._config()
            if workspace_id not in roots:
                raise BrokerError.not_found()
            del roots[workspace_id]
            self._write(roots, layer)

    def projects(
        self, workspace_id: str, *, activity_only: bool = False, username: str = ""
    ) -> list[dict]:
        if workspace_id == OTHER_WORKSPACE:
            return outside_projects(self, username)
        _validate_workspace_id(workspace_id)
        with self._lock:
            roots, _ = self._config()
            if workspace_id not in roots:
                raise BrokerError.not_found()
            projects = [
                project
                for project in self.browse(roots[workspace_id])["entries"]
                if not project["name"].startswith(".")
            ]
        activity = {}
        session_activity = {}
        if activity_only:
            cutoff = int(time.time()) - 48 * 3600
            cursor = None
            for _ in range(_SESSION_HISTORY_LIMIT):
                page = self._activity_request(
                    "thread/list",
                    {
                        "limit": _SESSION_PAGE_SIZE,
                        "cursor": cursor,
                        "sortKey": "updated_at",
                        "sortDirection": "desc",
                        "modelProviders": [],
                        "sourceKinds": _THREAD_SOURCES,
                        "archived": False,
                        "useStateDbOnly": True,
                    },
                )
                for thread in page.get("data", []):
                    cwd, updated = thread.get("cwd"), thread.get("updatedAt")
                    if isinstance(cwd, str) and isinstance(updated, int):
                        key = os.path.normcase(os.path.realpath(cwd))
                        activity[key] = max(activity.get(key, 0), updated)
                        status = thread.get("status")
                        status = status if isinstance(status, dict) else {}
                        thread_id = thread.get("id")
                        if (
                            isinstance(thread_id, str)
                            and len(thread_id) <= _SESSION_ID_LIMIT
                            and (updated >= cutoff or status.get("type") == "active")
                        ):
                            entries = session_activity.setdefault(key, [])
                            if len(entries) < 500:
                                entries.append(
                                    {
                                        "threadId": thread_id,
                                        "status": status.get("type", "unknown"),
                                        "needsInput": any(
                                            flag
                                            in (
                                                "waitingOnApproval",
                                                "waitingOnUserInput",
                                            )
                                            for flag in status.get("activeFlags", [])
                                        ),
                                        "updatedAt": updated,
                                    }
                                )
                cursor = page.get("nextCursor")
                if not cursor:
                    break
        for project in projects:
            key = os.path.normcase(os.path.realpath(project["path"]))
            project["updatedAt"] = activity.get(key, 0)
            project["sessionActivity"] = session_activity.get(key, [])
        return sorted(
            projects, key=lambda project: (-project["updatedAt"], project["name"])
        )

    def create_project(self, workspace_id: str, name: str) -> None:
        if not _valid_project_name(name):
            raise BrokerError.invalid_request(
                "A single project directory name is required"
            )
        with self._lock:
            roots, _ = self._config()
            if workspace_id not in roots:
                raise BrokerError.not_found()
            try:
                root = Path(roots[workspace_id]).resolve(strict=True)
                project = (root / name).resolve()
                if project.parent != root:
                    raise BrokerError.invalid_request(
                        "Project must be inside its workspace"
                    )
                project.mkdir(exist_ok=True)
            except (OSError, ValueError, RuntimeError) as error:
                raise BrokerError.invalid_request(
                    "Project directory cannot be created"
                ) from error

    def clone_project(
        self,
        workspace_id: str,
        remote_url: str,
        name: str | None = None,
        source: str = "https",
    ) -> str:
        if source == "https":
            clone_source = remote_url
            suggested_name = _public_https_repository_name(remote_url)
        elif source == "github":
            clone_source, suggested_name = _github_repository(remote_url)
        else:
            raise BrokerError.invalid_request("Unsupported repository clone source")
        project_name = suggested_name if name in (None, "") else name
        if not _valid_project_name(project_name) or ":" in project_name:
            raise BrokerError.invalid_request(
                "A single project directory name is required"
            )
        if not self._project_clone_slots.acquire(blocking=False):
            raise BrokerError.conflict("Too many repository clones are running")
        try:
            with self._lock:
                roots, _ = self._config()
                if workspace_id not in roots:
                    raise BrokerError.not_found()
                root = Path(roots[workspace_id]).resolve(strict=True)
                project_candidate = root / project_name
                if project_candidate.is_symlink():
                    raise BrokerError.invalid_request(
                        "Project directory cannot be a symlink"
                    )
                project = project_candidate.resolve()
                if project.parent != root:
                    raise BrokerError.invalid_request(
                        "Project must be inside its workspace"
                    )
                if project.exists() or project.is_symlink():
                    raise BrokerError.conflict("Project directory already exists")

            try:
                with tempfile.TemporaryDirectory(
                    prefix=_PROJECT_CLONE_TEMP_PREFIX, dir=root
                ) as temporary:
                    temporary_path = Path(temporary)
                    config = temporary_path / "gitconfig"
                    config.touch(mode=0o600)
                    isolated_home = temporary_path / "home"
                    isolated_home.mkdir(mode=0o700)
                    netrc = isolated_home / ".netrc"
                    netrc.touch(mode=0o600)
                    clone_path = temporary_path / "repository"
                    environment = {
                        key: value
                        for key, value in os.environ.items()
                        if not key.startswith("GIT_")
                    }
                    if source == "https":
                        environment["HOME"] = str(isolated_home)
                        environment["USERPROFILE"] = str(isolated_home)
                        environment["APPDATA"] = str(
                            isolated_home / "AppData" / "Roaming"
                        )
                        environment["LOCALAPPDATA"] = str(
                            isolated_home / "AppData" / "Local"
                        )
                        environment["XDG_CONFIG_HOME"] = str(isolated_home / ".config")
                        environment["CURL_HOME"] = str(isolated_home)
                        environment["NETRC"] = str(netrc)
                        environment.pop("SSH_ASKPASS", None)
                        environment["SSH_ASKPASS_REQUIRE"] = "never"
                        environment["GIT_CONFIG_GLOBAL"] = str(config)
                        environment["GIT_CONFIG_NOSYSTEM"] = "1"
                        environment["GIT_CEILING_DIRECTORIES"] = str(temporary_path)
                        command = [
                            "git",
                            "-c",
                            "credential.helper=",
                            "-c",
                            "protocol.allow=never",
                            "-c",
                            "protocol.https.allow=always",
                            "clone",
                            "--quiet",
                            "--",
                            clone_source,
                            str(clone_path),
                        ]
                    else:
                        environment["GH_PROMPT_DISABLED"] = "1"
                        command = [
                            "gh",
                            "repo",
                            "clone",
                            clone_source,
                            str(clone_path),
                        ]
                    environment["GIT_TERMINAL_PROMPT"] = "0"
                    try:
                        result = subprocess.run(
                            command,
                            cwd=temporary_path,
                            env=environment,
                            stdin=subprocess.DEVNULL,
                            stdout=subprocess.DEVNULL,
                            stderr=subprocess.DEVNULL,
                            timeout=_PROJECT_CLONE_TIMEOUT,
                            check=False,
                        )
                    except FileNotFoundError as error:
                        raise BrokerError.unavailable(
                            "GitHub CLI (gh) is not installed on the host"
                            if source == "github"
                            else "Git is not installed on the host"
                        ) from error
                    except subprocess.TimeoutExpired as error:
                        raise BrokerError.unavailable(
                            "Repository clone timed out"
                        ) from error
                    if result.returncode != 0:
                        raise BrokerError.invalid_request(
                            "GitHub clone failed; verify gh is authenticated and can access the repository"
                            if source == "github"
                            else "Git clone failed; verify the public HTTPS URL"
                        )

                    with self._lock:
                        roots, _ = self._config()
                        if workspace_id not in roots:
                            raise BrokerError.not_found()
                        if Path(roots[workspace_id]).resolve(strict=True) != root:
                            raise BrokerError.conflict(
                                "Workspace root changed during repository clone"
                            )
                        project_candidate = root / project_name
                        if project_candidate.is_symlink():
                            raise BrokerError.invalid_request(
                                "Project directory cannot be a symlink"
                            )
                        if project_candidate.resolve().parent != root:
                            raise BrokerError.invalid_request(
                                "Project must be inside its workspace"
                            )
                        if project.exists() or project.is_symlink():
                            raise BrokerError.conflict(
                                "Project directory already exists"
                            )
                        clone_path.rename(project)
                    return project_name
            except BrokerError:
                raise
            except (OSError, RuntimeError, ValueError) as error:
                raise BrokerError.invalid_request(
                    "Project directory cannot be created"
                ) from error
        finally:
            self._project_clone_slots.release()

    def _project_path(self, workspace_id: str, project_name: str) -> Path:
        if workspace_id == OTHER_WORKSPACE:
            return outside_directory(project_name)
        _validate_workspace_id(workspace_id)
        if not _valid_project_name(project_name):
            raise BrokerError.invalid_request(
                "A single project directory name is required"
            )
        roots, _ = self._config()
        if workspace_id not in roots:
            raise BrokerError.not_found()
        try:
            root = Path(roots[workspace_id]).resolve(strict=True)
            project = root / project_name
            if project.is_symlink():
                raise BrokerError.invalid_request(
                    "Project directory cannot be a symlink"
                )
            project = project.resolve(strict=True)
            if project.parent != root or not project.is_dir():
                raise BrokerError.invalid_request("Project directory is unavailable")
            return project
        except (OSError, ValueError, RuntimeError) as error:
            raise BrokerError.invalid_request(
                "Project directory is unavailable"
            ) from error

    def _thread_for_project(self, thread_id: str, project: Path) -> dict:
        if (
            not isinstance(thread_id, str)
            or not 1 <= len(thread_id) <= _SESSION_ID_LIMIT
        ):
            raise BrokerError.invalid_request("Invalid session ID")
        response = self._request(
            "thread/read", {"threadId": thread_id, "includeTurns": False}
        )
        thread = response.get("thread")
        cwd = thread.get("cwd") if isinstance(thread, dict) else None
        if (
            not isinstance(thread, dict)
            or thread.get("id") != thread_id
            or not isinstance(cwd, str)
        ):
            raise BrokerError.not_found()
        try:
            if Path(cwd).resolve(strict=True) != project:
                raise BrokerError.not_found()
        except (OSError, ValueError, RuntimeError) as error:
            raise BrokerError.not_found() from error
        return thread

    def project_sessions(
        self,
        username: str,
        workspace_id: str,
        project_name: str,
        *,
        include_history: bool = False,
    ) -> list[dict]:
        with self._lock:
            project = self._project_path(workspace_id, project_name)
            closed = set(self._closed_sessions.get(username, []))
            if workspace_id == OTHER_WORKSPACE and not include_history:
                return [
                    {**session, "archived": False}
                    for entry in outside_projects(self, username)
                    if entry["path"] == str(project)
                    for session in entry["sessionActivity"]
                ]
            cutoff = int(time.time()) - 48 * 3600
            sessions = []
            for archived in [False, True] if include_history else [False]:
                cursor = None
                for _ in range(_SESSION_HISTORY_LIMIT):
                    page = self._request(
                        "thread/list",
                        {
                            "limit": _SESSION_PAGE_SIZE,
                            "cursor": cursor,
                            "sortKey": "updated_at",
                            "sortDirection": "desc",
                            "cwd": str(project),
                            "modelProviders": [],
                            "sourceKinds": _THREAD_SOURCES,
                            "archived": archived,
                        },
                    )
                    for item in page.get("data", []):
                        thread_id = item.get("id")
                        cwd = item.get("cwd")
                        updated = item.get("updatedAt", 0)
                        if not isinstance(thread_id, str) or not isinstance(cwd, str):
                            continue
                        try:
                            if Path(cwd).resolve(strict=True) != project:
                                continue
                        except (OSError, ValueError, RuntimeError):
                            continue
                        if (
                            not include_history
                            and (not isinstance(updated, int) or updated < cutoff)
                            and (
                                not isinstance(item.get("status"), dict)
                                or item["status"].get("type") != "active"
                            )
                        ):
                            continue
                        if not include_history and thread_id in closed:
                            continue
                        sessions.append(
                            {
                                "threadId": thread_id,
                                "title": str(
                                    item.get("name") or item.get("preview") or thread_id
                                )[:256],
                                "archived": archived,
                                "updatedAt": updated if isinstance(updated, int) else 0,
                                "status": item.get("status", {}).get("type", "unknown")
                                if isinstance(item.get("status"), dict)
                                else "unknown",
                                "needsInput": any(
                                    flag in ("waitingOnApproval", "waitingOnUserInput")
                                    for flag in item.get("status", {}).get(
                                        "activeFlags", []
                                    )
                                )
                                if isinstance(item.get("status"), dict)
                                else False,
                            }
                        )
                    cursor = page.get("nextCursor")
                    if not cursor or len(sessions) >= 500:
                        break
                if len(sessions) >= 500:
                    break
            sessions.sort(key=lambda session: session["updatedAt"], reverse=True)
            return sessions[:500]

    def new_session(self, username: str, workspace_id: str, project_name: str) -> dict:
        with self._work_admission.admit(), self._lock:
            project = self._project_path(workspace_id, project_name)
            response = self._request("thread/start", {"cwd": str(project)})
            thread = response.get("thread")
            thread_id = thread.get("id") if isinstance(thread, dict) else None
            if not isinstance(thread_id, str):
                raise BrokerError.internal()
            self._request("thread/webUi/open", {"threadId": thread_id})
            self._remember_closed(username, thread_id, False)
            return {
                "threadId": thread_id,
                "title": str(thread.get("name") or thread_id)[:256],
            }

    def resume_session(
        self,
        username: str,
        workspace_id: str,
        project_name: str,
        thread_id: str,
        *,
        archived: bool = False,
    ) -> dict:
        with self._work_admission.admit(), self._lock:
            project = self._project_path(workspace_id, project_name)
            self._thread_for_project(thread_id, project)
            if archived:
                self._request("thread/unarchive", {"threadId": thread_id})
            response = self._request(
                "thread/resume",
                {
                    "threadId": thread_id,
                    "excludeTurns": True,
                },
            )
            thread = response.get("thread")
            if not isinstance(thread, dict) or thread.get("id") != thread_id:
                raise BrokerError.not_found()
            self._remember_closed(username, thread_id, False)
            return {
                "threadId": thread_id,
                "title": str(thread.get("name") or thread.get("preview") or thread_id)[
                    :256
                ],
            }

    def session_data(
        self,
        username: str,
        workspace_id: str,
        project_name: str,
        thread_id: str,
        cursor: str | None = None,
    ) -> dict:
        with self._lock:
            project = self._project_path(workspace_id, project_name)
            if thread_id in self._closed_sessions.get(username, []):
                raise BrokerError.not_found()
            thread = self._thread_for_project(thread_id, project)
            params = {
                "threadId": thread_id,
                "limit": _WEB_TURN_LIMIT,
                "sortDirection": "desc",
                "itemsView": "notLoaded",
            }
            if cursor:
                if not isinstance(cursor, str) or len(cursor) > 4096:
                    raise BrokerError.invalid_request("Invalid transcript cursor")
                params["cursor"] = cursor
            page = self._request("thread/turns/list", params)
            turns = page.get("data", [])
            if not isinstance(turns, list):
                turns = []
            for turn in turns:
                if not isinstance(turn, dict):
                    continue
                turn["items"] = []
                item_cursor = None
                item_limit = _WEB_ITEM_LIMIT
                while len(turn["items"]) < _WEB_ITEM_LIMIT:
                    item_params = {
                        "threadId": thread_id,
                        "turnId": turn["id"],
                        "limit": min(item_limit, _WEB_ITEM_LIMIT - len(turn["items"])),
                        "sortDirection": "asc",
                        "cursor": item_cursor,
                    }
                    try:
                        item_page = self._request("thread/items/list", item_params)
                    except BrokerError as error:
                        if item_params[
                            "limit"
                        ] > 1 and "message exceeds the size limit" in str(
                            error.__cause__
                        ):
                            item_limit = max(1, item_params["limit"] // 2)
                            continue
                        raise
                    entries = item_page.get("data", [])
                    turn["items"].extend(
                        entry["item"]
                        for entry in entries
                        if isinstance(entry, dict)
                        and isinstance(entry.get("item"), dict)
                    )
                    next_cursor = item_page.get("nextCursor")
                    if not entries or not next_cursor or next_cursor == item_cursor:
                        break
                    item_cursor = next_cursor
            live_thread = {"turns": turns}
            self._live_items.apply(thread_id, live_thread)
            remaining = _WEB_TRANSCRIPT_LIMIT
            displayed = []
            for turn in reversed(live_thread["turns"]):
                if not isinstance(turn, dict):
                    continue
                items = []
                for item in turn.get("items", [])[:_WEB_ITEM_LIMIT]:
                    display = self._web_item(item, remaining)
                    if display is None:
                        continue
                    remaining -= sum(
                        len(value)
                        for value in display.values()
                        if isinstance(value, str)
                    )
                    items.append(display)
                    if remaining <= 0:
                        break
                displayed.append(
                    {
                        "id": str(turn.get("id", "")),
                        "status": turn.get("status", "unknown"),
                        "items": items,
                    }
                )
                if remaining <= 0:
                    break
            return {
                "thread": {
                    "id": thread_id,
                    "title": str(
                        thread.get("name") or thread.get("preview") or thread_id
                    )[:256],
                    "status": thread.get("status", {}).get("type", "unknown")
                    if isinstance(thread.get("status"), dict)
                    else "unknown",
                },
                "turns": displayed,
                "hasOlder": bool(page.get("nextCursor")),
                "nextCursor": page.get("nextCursor"),
            }

    @staticmethod
    def _web_item(item: dict, remaining: int) -> dict | None:
        if not isinstance(item, dict) or remaining <= 0:
            return None
        kind = item.get("type")
        if kind == "userMessage":
            pieces = [
                value.get("text", "")
                if value.get("type") == "text"
                else "[Image or attachment]"
                for value in item.get("content", [])
                if isinstance(value, dict)
            ]
            return {
                "type": kind,
                "text": "\n".join(pieces)[: min(_WEB_TEXT_LIMIT, remaining)],
            }
        if kind == "agentMessage":
            return {
                "type": kind,
                "text": str(item.get("text", ""))[: min(_WEB_TEXT_LIMIT, remaining)],
            }
        if kind == "plan":
            return {
                "type": kind,
                "text": str(item.get("text", ""))[: min(_WEB_TEXT_LIMIT, remaining)],
            }
        if kind == "commandExecution":
            command = str(item.get("command", ""))[:4096]
            output = str(item.get("aggregatedOutput") or "")[
                : max(0, min(8192, remaining - len(command)))
            ]
            return {"type": "tool", "label": "Ran", "text": command, "output": output}
        if kind in ("mcpToolCall", "dynamicToolCall"):
            tool = str(item.get("tool", "tool"))[:256]
            server = str(item.get("server", ""))[:256]
            status = item.get("status")
            return {
                "type": "tool",
                "label": f"{server} {tool}".strip() or "Tool",
                "text": str(status.get("type", "completed"))[:256]
                if isinstance(status, dict)
                else "completed",
            }
        if kind == "fileChange":
            changes = item.get("changes", [])
            names = [
                str(change.get("path", ""))
                for change in changes[:32]
                if isinstance(change, dict) and isinstance(change.get("path"), str)
            ]
            return {
                "type": "tool",
                "label": "File changes",
                "text": "\n".join(names)[: min(8192, remaining)],
            }
        return None

    def close_session(
        self, username: str, workspace_id: str, project_name: str, thread_id: str
    ) -> None:
        with self._lock:
            project = self._project_path(workspace_id, project_name)
            self._thread_for_project(thread_id, project)
            self._remember_closed(username, thread_id, True)

    def start_turn(
        self,
        username: str,
        workspace_id: str,
        project_name: str,
        thread_id: str,
        message: str,
    ) -> dict:
        if (
            not isinstance(message, str)
            or not message.strip()
            or len(message) > 64 * 1024
        ):
            raise BrokerError.invalid_request("Message must contain 1–64 KiB of text")
        with self._work_admission.admit(), self._lock:
            project = self._project_path(workspace_id, project_name)
            if thread_id in self._closed_sessions.get(username, []):
                raise BrokerError.not_found()
            self._thread_for_project(thread_id, project)
            response = self._request(
                "turn/start",
                {
                    "threadId": thread_id,
                    "input": [{"type": "text", "text": message}],
                },
            )
            turn = response.get("turn")
            turn_id = (
                turn.get("id", turn.get("turnId"))
                if isinstance(turn, dict)
                else response.get("turnId")
            )
            if isinstance(turn_id, str):
                self._active_turns[thread_id] = turn_id
            return response

    def close(self) -> None:
        with self._lock:
            self._closed = True
            if self._client is not None:
                self._client.close()
                self._client = None
        with self._activity_lock:
            if self._activity_client is not None:
                self._activity_client.close()
                self._activity_client = None
