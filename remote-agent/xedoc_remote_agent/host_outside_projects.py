"""Session-backed projects outside configured workspace roots."""

import os
import time
from pathlib import Path

from .errors import BrokerError, ErrorCode


OTHER_WORKSPACE = "@other"


def outside_directory(value: str) -> Path:
    if (
        not isinstance(value, str)
        or len(value) > 16384
        or not Path(value).is_absolute()
    ):
        raise BrokerError.invalid_request("An absolute directory path is required")
    try:
        directory = Path(value).resolve(strict=True)
        if not directory.is_dir() or not os.access(directory, os.R_OK | os.X_OK):
            raise OSError
        return directory
    except (OSError, ValueError, RuntimeError) as error:
        raise BrokerError.invalid_request("Directory is unavailable") from error


def session_inventory(service, username: str, roots: dict, *, include_outside: bool):
    from .host_workspace import _THREAD_SOURCES

    closed = set(service._closed_sessions.get(username, []))
    threads = {}
    cursor = None
    cutoff = int(time.time()) - 48 * 3600
    for _ in range(10):
        page = service._activity_request(
            "thread/list",
            {
                "limit": 100,
                "cursor": cursor,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "modelProviders": [],
                "sourceKinds": _THREAD_SOURCES,
                "archived": False,
                "useStateDbOnly": True,
            },
        )
        outside_window_exhausted = False
        for thread in page.get("data", []):
            if not isinstance(thread, dict) or not isinstance(thread.get("id"), str):
                continue
            updated = thread.get("updatedAt")
            if include_outside and (not isinstance(updated, int) or updated < cutoff):
                outside_window_exhausted = True
                continue
            threads[thread["id"]] = thread
        cursor = page.get("nextCursor")
        if not cursor or outside_window_exhausted:
            break
    loaded = set()
    if include_outside:
        cursor = None
        for _ in range(10):
            page = service._activity_request(
                "thread/loaded/list", {"limit": 100, "cursor": cursor}
            )
            loaded.update(
                value for value in page.get("data", []) if isinstance(value, str)
            )
            cursor = page.get("nextCursor")
            if not cursor:
                break
        for thread_id in loaded - closed - threads.keys():
            try:
                thread = service._activity_request(
                    "thread/read", {"threadId": thread_id, "includeTurns": False}
                ).get("thread")
            except BrokerError as error:
                if error.code is ErrorCode.NOT_FOUND:
                    continue
                raise
            if isinstance(thread, dict):
                threads[thread_id] = thread
    canonical_roots = {
        key: Path(os.path.realpath(value)) for key, value in roots.items()
    }
    result = {key: [] for key in roots}
    if include_outside:
        result[OTHER_WORKSPACE] = []
    for thread_id, thread in threads.items():
        cwd = thread.get("cwd")
        if (
            thread_id in closed
            or not isinstance(cwd, str)
            or not Path(cwd).is_absolute()
        ):
            continue
        directory = Path(os.path.realpath(cwd))
        workspace_id = next(
            (
                key
                for key, root in canonical_roots.items()
                if directory.parent == root and not directory.name.startswith(".")
            ),
            None,
        )
        status = thread.get("status")
        status = status if isinstance(status, dict) else {}
        updated = thread.get("updatedAt")
        updated = updated if isinstance(updated, int) else 0
        if workspace_id is None:
            if (
                not include_outside
                or (updated < cutoff and status.get("type") != "active")
            ):
                continue
            workspace_id, project_name = OTHER_WORKSPACE, str(directory)
        else:
            if directory.name.startswith(".") or (
                updated < cutoff and status.get("type") != "active"
            ):
                continue
            project_name = directory.name
        result[workspace_id].append(
            {
                "threadId": thread_id,
                "title": str(thread.get("name") or thread.get("preview") or thread_id)[
                    :256
                ],
                "projectName": project_name,
                "status": status.get("type", "unknown"),
                "needsInput": any(
                    flag in ("waitingOnApproval", "waitingOnUserInput")
                    for flag in status.get("activeFlags", [])
                ),
                "updatedAt": updated,
            }
        )
    return result


def outside_projects(service, username: str) -> list[dict]:
    roots, _ = service._config()
    sessions = session_inventory(service, username, roots, include_outside=True)[
        OTHER_WORKSPACE
    ]
    projects = {}
    for session in sessions:
        path = session["projectName"]
        project = projects.setdefault(
            path,
            {
                "name": path,
                "label": Path(path).name or path,
                "path": path,
                "updatedAt": 0,
                "sessionActivity": [],
            },
        )
        project["updatedAt"] = max(project["updatedAt"], session["updatedAt"])
        project["sessionActivity"].append(session)
    return sorted(
        projects.values(), key=lambda project: (-project["updatedAt"], project["name"])
    )
