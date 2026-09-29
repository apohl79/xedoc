"""Bounded session inventory projections over controller list/search responses."""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

from .errors import BrokerError
from .models import (
    MAX_CURSOR_LENGTH,
    MAX_ID_LENGTH,
    MAX_SNIPPET_LENGTH,
    MAX_TITLE_LENGTH,
    SearchPage,
    SearchResult,
    SessionPage,
    SessionSummary,
)
from .workspaces import WorkspaceRegistry


MAX_PAGE_SIZE = 100
DEFAULT_PAGE_SIZE = 50
MAX_SEARCH_BYTES = 4096


class SessionCatalog:
    """Normalize controller pages and discard sessions outside the allowlist."""

    def __init__(
        self,
        controller: Any,
        workspaces: WorkspaceRegistry | None = None,
        *,
        host_id: str | None = None,
        max_result_bytes: int | None = None,
    ) -> None:
        self.controller = controller
        self.workspaces = workspaces or getattr(controller, "workspaces", None)
        if not isinstance(self.workspaces, WorkspaceRegistry):
            raise BrokerError.invalid_request()
        self.host_id = host_id or getattr(controller, "host_id", "host_local")
        value = max_result_bytes or getattr(
            getattr(controller, "limits", None), "max_result_bytes", 65536
        )
        if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
            raise BrokerError.invalid_request()
        self.max_result_bytes = value

    def workspace_list(self) -> tuple[dict[str, str], ...]:
        return self.workspaces.public_list()

    def list_sessions(
        self, *, cursor: str | None = None, limit: int | None = None
    ) -> SessionPage:
        page_size = _page_size(limit)
        cursor = _cursor(cursor)
        response = self._call_list(cursor, page_size)
        records = [
            session
            for raw in _records(response)
            if (session := self._normalize_session(raw)) is not None
        ]
        next_cursor = _next_cursor(response)
        return _fit_session_page(records, next_cursor, self.max_result_bytes)

    def search_sessions(
        self,
        search_term: str,
        *,
        cursor: str | None = None,
        limit: int | None = None,
    ) -> SearchPage:
        if not isinstance(search_term, str) or not search_term.strip():
            raise BrokerError.invalid_request()
        if len(search_term.encode("utf-8")) > MAX_SEARCH_BYTES:
            raise BrokerError.limit_exceeded()
        page_size = _page_size(limit)
        cursor = _cursor(cursor)
        response = self._call_search(search_term, cursor, page_size)
        results: list[SearchResult] = []
        for raw in _records(response):
            if not isinstance(raw, dict):
                continue
            session = self._normalize_session(raw.get("thread", raw))
            if session is None:
                continue
            snippet = raw.get("snippet", "")
            if not isinstance(snippet, str):
                snippet = ""
            results.append(SearchResult(session, _text(snippet, MAX_SNIPPET_LENGTH)))
        return _fit_search_page(results, _next_cursor(response), self.max_result_bytes)

    # Short aliases make the catalog convenient for local IPC dispatch.
    list = list_sessions
    search = search_sessions

    def _call_list(self, cursor: str | None, limit: int) -> dict[str, Any]:
        try:
            if hasattr(self.controller, "_list_sessions"):
                return self.controller._list_sessions(cursor=cursor, limit=limit)
            if not hasattr(self.controller, "list_sessions"):
                raise BrokerError.internal()
            return self.controller.list_sessions(cursor=cursor, limit=limit)
        except BrokerError:
            raise
        except BaseException as error:
            from .errors import map_controller_error

            raise map_controller_error(error) from error

    def _call_search(
        self, search_term: str, cursor: str | None, limit: int
    ) -> dict[str, Any]:
        try:
            if hasattr(self.controller, "_search_sessions"):
                return self.controller._search_sessions(
                    search_term, cursor=cursor, limit=limit
                )
            if not hasattr(self.controller, "search_sessions"):
                raise BrokerError.internal()
            return self.controller.search_sessions(
                search_term, cursor=cursor, limit=limit
            )
        except BrokerError:
            raise
        except BaseException as error:
            from .errors import map_controller_error

            raise map_controller_error(error) from error

    def _normalize_session(self, value: Any) -> SessionSummary | None:
        if not isinstance(value, dict):
            return None
        thread_id = value.get("id", value.get("threadId"))
        cwd = value.get("cwd")
        if not isinstance(thread_id, str) or not thread_id or len(thread_id) > MAX_ID_LENGTH:
            return None
        if not isinstance(cwd, str) or not cwd or self.workspaces.workspace_for_path(cwd) is None:
            return None
        try:
            canonical_cwd = str(Path(cwd).resolve(strict=False))
        except (OSError, RuntimeError, ValueError):
            return None
        if not self.workspaces.contains(canonical_cwd):
            return None
        running = value.get("isRunning")
        if not isinstance(running, bool):
            status = value.get("status")
            running = (
                isinstance(status, dict) and status.get("type") in {"active", "inProgress"}
            ) or (isinstance(status, str) and status in {"active", "inProgress"})
        last_activity = value.get("lastActivity", value.get("updatedAt"))
        if not isinstance(last_activity, int) or isinstance(last_activity, bool):
            return None
        raw_summary = value.get("summary")
        title = value.get("title", value.get("name"))
        if isinstance(raw_summary, dict) and isinstance(raw_summary.get("title"), str):
            title = raw_summary["title"]
        summary = {"title": _text(title, MAX_TITLE_LENGTH)} if isinstance(title, str) else {}
        return SessionSummary(
            host_id=self.host_id,
            thread_id=thread_id,
            cwd=canonical_cwd,
            is_running=running,
            last_activity=last_activity,
            summary=summary,
        )


def _page_size(value: int | None) -> int:
    if value is None:
        return DEFAULT_PAGE_SIZE
    if not isinstance(value, int) or isinstance(value, bool) or not 0 < value <= MAX_PAGE_SIZE:
        raise BrokerError.limit_exceeded()
    return value


def _cursor(value: str | None) -> str | None:
    if value is not None and (
        not isinstance(value, str) or not value or len(value) > MAX_CURSOR_LENGTH
    ):
        raise BrokerError.invalid_request()
    return value


def _records(response: Any) -> list[Any]:
    if not isinstance(response, dict) or not isinstance(response.get("data"), list):
        raise BrokerError.internal()
    return response["data"]


def _next_cursor(response: dict[str, Any]) -> str | None:
    value = response.get("nextCursor", response.get("next_cursor"))
    if value is None:
        return None
    if not isinstance(value, str) or len(value) > MAX_CURSOR_LENGTH:
        raise BrokerError.limit_exceeded()
    return value


def _text(value: str, limit: int) -> str:
    encoded = value.encode("utf-8", "replace")
    if len(encoded) <= limit:
        return value
    return encoded[:limit].decode("utf-8", "ignore")


def _fit_session_page(
    records: list[SessionSummary], cursor: str | None, max_bytes: int
) -> SessionPage:
    while True:
        page = SessionPage(tuple(records), cursor)
        if _size(page.to_dict()) <= max_bytes:
            return page
        if not records:
            raise BrokerError.limit_exceeded()
        records.pop()


def _fit_search_page(
    records: list[SearchResult], cursor: str | None, max_bytes: int
) -> SearchPage:
    while True:
        page = SearchPage(tuple(records), cursor)
        if _size(page.to_dict()) <= max_bytes:
            return page
        if not records:
            raise BrokerError.limit_exceeded()
        records.pop()


def _size(value: Any) -> int:
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))
