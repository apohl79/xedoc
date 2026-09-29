"""Bootstrap/config loading and canonical workspace containment."""

from __future__ import annotations

import json
import os
from pathlib import Path, PurePosixPath, PureWindowsPath
import re
from typing import Any, Mapping
from urllib.parse import urlsplit

from .errors import BrokerError
from .models import BootstrapDescriptor, BrokerConfig, Workspace


MAX_BOOTSTRAP_BYTES = 64 * 1024
MAX_RELATIVE_PATH_BYTES = 16 * 1024
_WORKSPACE_ID = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9_-]{0,63})?\Z")


def load_bootstrap_descriptor(
    path: str | os.PathLike[str] | None = None,
    *,
    xedoc_home: str | os.PathLike[str] | None = None,
) -> BootstrapDescriptor:
    """Read the local controller endpoint from the host-only bootstrap file."""

    bootstrap_path = (
        Path(path)
        if path is not None
        else _xedoc_home(xedoc_home) / "remote-agent" / "bootstrap.toml"
    )
    try:
        if not bootstrap_path.is_file():
            raise OSError
        if bootstrap_path.stat().st_size > MAX_BOOTSTRAP_BYTES:
            raise ValueError
        text = bootstrap_path.read_text(encoding="utf-8")
    except (OSError, UnicodeError, ValueError) as error:
        raise BrokerError.unavailable() from error
    try:
        return parse_bootstrap_descriptor(text)
    except BrokerError:
        raise
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error


def parse_bootstrap_descriptor(text: str) -> BootstrapDescriptor:
    """Parse the intentionally tiny bootstrap TOML subset without ``tomllib``."""

    if not isinstance(text, str) or len(text.encode("utf-8")) > MAX_BOOTSTRAP_BYTES:
        raise BrokerError.limit_exceeded()
    controller: str | None = None
    section: str | None = None
    for raw_line in text.splitlines():
        line = _strip_toml_comment(raw_line).strip()
        if not line:
            continue
        if line.startswith("[") or "=" not in line:
            raise BrokerError.invalid_request()
        key, raw_value = (part.strip() for part in line.split("=", 1))
        if section is not None or key != "controller" or controller is not None:
            raise BrokerError.invalid_request()
        try:
            value = json.loads(raw_value)
        except json.JSONDecodeError as error:
            raise BrokerError.invalid_request() from error
        if not isinstance(value, str):
            raise BrokerError.invalid_request()
        controller = value
    if controller is None:
        raise BrokerError.invalid_request()
    return _parse_controller_endpoint(controller)


def load_broker_config(response: Mapping[str, Any]) -> BrokerConfig:
    """Validate the untrusted ``config/read`` response.

    The caller must have made ``config/read`` with no cwd. This function also
    rejects response shapes that carry project-layer metadata, so a caller
    cannot accidentally turn a project setting into host authority.
    """

    if not isinstance(response, Mapping):
        raise BrokerError.invalid_request()
    _reject_project_layers(response)
    raw_config = response.get("config", response)
    if not isinstance(raw_config, Mapping):
        raise BrokerError.invalid_request()
    raw_remote = raw_config.get("remote_agent")
    if raw_remote is None:
        # Stage 3 may not have added the Rust config section yet. This is a
        # bounded configuration error, not permission to use a fallback list.
        raise BrokerError.invalid_request("remote-agent configuration unavailable")
    if not isinstance(raw_remote, Mapping):
        raise BrokerError.invalid_request()
    workspace_mapping = raw_remote.get("workspaces")
    if not isinstance(workspace_mapping, Mapping):
        raise BrokerError.invalid_request()
    registry = WorkspaceRegistry.from_mapping(workspace_mapping)
    return BrokerConfig.from_mapping(raw_remote, registry.workspaces)


class WorkspaceRegistry:
    """Canonical workspace roots and safe relative-path resolution."""

    def __init__(self, workspaces: tuple[Workspace, ...]) -> None:
        if not workspaces:
            raise BrokerError.invalid_request()
        ids: set[str] = set()
        roots: set[str] = set()
        for workspace in workspaces:
            _validate_workspace_id(workspace.workspace_id)
            canonical = _canonical_root(workspace.root)
            normalized = os.path.normcase(os.fspath(canonical))
            if workspace.workspace_id in ids or normalized in roots:
                raise BrokerError.invalid_request()
            ids.add(workspace.workspace_id)
            roots.add(normalized)
        self.workspaces = tuple(
            Workspace(workspace.workspace_id, _canonical_root(workspace.root))
            for workspace in workspaces
        )
        self._by_id = {workspace.workspace_id: workspace for workspace in self.workspaces}

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "WorkspaceRegistry":
        if not isinstance(value, Mapping) or not value:
            raise BrokerError.invalid_request()
        workspaces: list[Workspace] = []
        for workspace_id, raw_root in value.items():
            if not isinstance(workspace_id, str) or not isinstance(raw_root, str):
                raise BrokerError.invalid_request()
            _validate_workspace_id(workspace_id)
            root = Path(raw_root)
            if not root.is_absolute():
                raise BrokerError.invalid_request()
            workspaces.append(Workspace(workspace_id, root))
        return cls(tuple(workspaces))

    def get(self, workspace_id: str) -> Workspace:
        if not isinstance(workspace_id, str):
            raise BrokerError.invalid_request()
        workspace = self._by_id.get(workspace_id)
        if workspace is None:
            raise BrokerError.not_found()
        return workspace

    def resolve_request(self, request: Mapping[str, Any]) -> Path:
        """Resolve exactly ``{workspaceId, relativePath}`` for session/start."""

        if not isinstance(request, Mapping):
            raise BrokerError.invalid_request()
        allowed = {"workspaceId", "relativePath"}
        if set(request) - allowed or "workspaceId" not in request:
            raise BrokerError.invalid_request()
        workspace_id = request["workspaceId"]
        relative_path = request.get("relativePath", "")
        if not isinstance(workspace_id, str) or not isinstance(relative_path, str):
            raise BrokerError.invalid_request()
        return self.resolve_relative(workspace_id, relative_path)

    def resolve_relative(self, workspace_id: str, relative_path: str = "") -> Path:
        workspace = self.get(workspace_id)
        if not isinstance(relative_path, str):
            raise BrokerError.invalid_request()
        if len(relative_path.encode("utf-8")) > MAX_RELATIVE_PATH_BYTES:
            raise BrokerError.limit_exceeded()
        if "\x00" in relative_path or "\\" in relative_path:
            raise BrokerError.invalid_request()
        posix = PurePosixPath(relative_path)
        windows = PureWindowsPath(relative_path)
        if posix.is_absolute() or windows.is_absolute() or windows.drive:
            raise BrokerError.invalid_request()
        if any(part == ".." for part in posix.parts):
            raise BrokerError.invalid_request()
        try:
            candidate = (workspace.root / posix).resolve(strict=False)
        except (OSError, RuntimeError, ValueError) as error:
            raise BrokerError.invalid_request() from error
        if not _is_within(candidate, workspace.root):
            raise BrokerError.invalid_request()
        try:
            if candidate.exists() and not candidate.is_dir():
                raise BrokerError.invalid_request()
        except OSError as error:
            raise BrokerError.unavailable() from error
        return candidate

    def workspace_for_path(self, path: str | os.PathLike[str]) -> Workspace | None:
        if not isinstance(path, (str, os.PathLike)):
            return None
        try:
            candidate = Path(path).resolve(strict=False)
        except (OSError, RuntimeError, ValueError):
            return None
        matches = [
            workspace
            for workspace in self.workspaces
            if _is_within(candidate, workspace.root)
        ]
        if not matches:
            return None
        return max(matches, key=lambda workspace: len(workspace.root.parts))

    def contains(self, path: str | os.PathLike[str]) -> bool:
        return self.workspace_for_path(path) is not None

    def public_list(self) -> tuple[dict[str, str], ...]:
        """Return workspace IDs without exposing host filesystem roots."""

        return tuple({"workspaceId": workspace.workspace_id} for workspace in self.workspaces)


def _xedoc_home(value: str | os.PathLike[str] | None) -> Path:
    if value is not None:
        home = Path(value)
    else:
        raw = os.environ.get("XEDOC_HOME")
        home = Path(raw) if raw else Path.home() / ".xedoc"
    if not home.is_absolute():
        raise BrokerError.invalid_request()
    return home


def _parse_controller_endpoint(value: str) -> BootstrapDescriptor:
    if "\x00" in value or "%" in value:
        raise BrokerError.invalid_request()
    parsed = urlsplit(value)
    if parsed.scheme != "unix" or parsed.netloc or parsed.query or parsed.fragment:
        raise BrokerError.invalid_request()
    if not parsed.path.startswith("/") or parsed.path == "/":
        raise BrokerError.invalid_request()
    socket_path = Path(parsed.path)
    if not socket_path.is_absolute():
        raise BrokerError.invalid_request()
    return BootstrapDescriptor(socket_path)


def _strip_toml_comment(line: str) -> str:
    quoted = False
    escaped = False
    for index, char in enumerate(line):
        if char == '"' and not escaped:
            quoted = not quoted
        if char == "#" and not quoted:
            return line[:index]
        escaped = char == "\\" and not escaped
        if char != "\\":
            escaped = False
    return line


def _validate_workspace_id(workspace_id: str) -> None:
    if not _WORKSPACE_ID.fullmatch(workspace_id):
        raise BrokerError.invalid_request()


def _canonical_root(root: Path) -> Path:
    try:
        if not root.is_absolute() or not root.is_dir() or not os.access(root, os.R_OK | os.X_OK):
            raise OSError
        canonical = root.resolve(strict=True)
        if not canonical.is_dir() or not os.access(canonical, os.R_OK | os.X_OK):
            raise OSError
    except (OSError, RuntimeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    return canonical


def _is_within(candidate: Path, root: Path) -> bool:
    try:
        candidate.relative_to(root)
    except ValueError:
        return False
    return True


def _reject_project_layers(response: Mapping[str, Any]) -> None:
    layers = response.get("layers")
    if layers not in (None, []):
        raise BrokerError.invalid_request()
    origins = response.get("origins")
    if not isinstance(origins, Mapping):
        return
    for key, value in origins.items():
        if not isinstance(key, str) or not key.startswith("remote_agent"):
            continue
        if "project" in repr(value).lower():
            raise BrokerError.invalid_request()
