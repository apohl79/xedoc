"""Bounded structured transcript items relayed to a coordinator's UI.

Items are the remote app-server's own ``ThreadItem`` JSON, so the coordinator
renders them exactly like a local thread. They travel next to the text
projection, never to a model: model-facing results strip them.
"""

from __future__ import annotations

import hashlib
import json
import math
from typing import Any, Mapping, Sequence

from .errors import BrokerError
from .models import MAX_ID_LENGTH


ITEM_TYPES = frozenset(
    {
        "userMessage",
        "agentMessage",
        "plan",
        "reasoning",
        "commandExecution",
        "fileChange",
        "mcpToolCall",
        "dynamicToolCall",
        "collabAgentToolCall",
        "webSearch",
    }
)
MAX_ENTRIES = 16
MAX_ITEM_BYTES = 12 * 1024
MAX_ENTRIES_BYTES = 24 * 1024
_STRING_LIMITS = (6 * 1024, 2 * 1024, 512)
_MAX_LIST_ITEMS = 32
_MAX_DEPTH = 6
_TAIL_KEYS = frozenset({"aggregatedOutput", "diff"})


def project_entry(turn_id: str, item: Any) -> dict[str, Any] | None:
    """Return a bounded ``{"turnId", "item"}`` entry, or None for an unsupported item."""

    if not isinstance(item, Mapping) or item.get("type") not in ITEM_TYPES:
        return None
    if not _is_identifier(turn_id) or not _is_identifier(item.get("id")):
        return None
    for limit in _STRING_LIMITS:
        entry = {"turnId": turn_id, "item": _bound(dict(item), limit, None, 0)}
        if _encoded_size(entry) <= MAX_ITEM_BYTES:
            return entry
    return None


def is_final(item: Mapping[str, Any]) -> bool:
    """Whether an item has reached the state a finished history cell shows."""

    if item.get("status") == "inProgress":
        return False
    item_type = item.get("type")
    if item_type in {"agentMessage", "plan"}:
        return bool(str(item.get("text") or "").strip())
    if item_type == "reasoning":
        return any(
            isinstance(part, str) and part.strip()
            for key in ("summary", "content")
            for part in (item.get(key) if isinstance(item.get(key), list) else [])
        )
    if item_type == "userMessage":
        return bool(item.get("content"))
    return True


def entry_key(entry: Mapping[str, Any]) -> tuple[str, str, str]:
    """Identify an entry independently of ids that differ between rollout and live views."""

    item = entry["item"]
    item_type = item["type"]
    if item_type in {"agentMessage", "plan"}:
        basis: Any = item.get("text")
    elif item_type == "reasoning":
        basis = [item.get("summary"), item.get("content")]
    elif item_type == "userMessage":
        basis = item.get("content")
    elif item_type == "fileChange":
        basis = [
            [change.get("path"), change.get("kind"), change.get("diff")]
            for change in item.get("changes", [])
            if isinstance(change, Mapping)
        ]
    else:
        basis = item["id"]
    digest = hashlib.sha256(
        json.dumps(basis, sort_keys=True, ensure_ascii=False).encode("utf-8")
    ).hexdigest()[:24]
    return entry["turnId"], item_type, digest


def take_within_budget(
    entries: Sequence[dict[str, Any]], max_entries: int = MAX_ENTRIES
) -> list[dict[str, Any]]:
    """Take a leading run of entries that fits one message."""

    taken: list[dict[str, Any]] = []
    total = 0
    for entry in entries:
        size = _encoded_size(entry)
        if taken and (len(taken) >= max_entries or total + size > MAX_ENTRIES_BYTES):
            break
        taken.append(entry)
        total += size
    return taken


def validate_entries(value: Any) -> list[dict[str, Any]]:
    """Validate relayed entries against the fixed entry shape and size bounds."""

    if not isinstance(value, list) or len(value) > MAX_ENTRIES:
        raise BrokerError.invalid_request()
    for entry in value:
        if (
            not isinstance(entry, Mapping)
            or set(entry) != {"turnId", "item"}
            or not _is_identifier(entry["turnId"])
            or not isinstance(entry["item"], Mapping)
            or entry["item"].get("type") not in ITEM_TYPES
            or not _is_identifier(entry["item"].get("id"))
            or _encoded_size(entry) > MAX_ITEM_BYTES
        ):
            raise BrokerError.invalid_request()
    if _encoded_size(value) > MAX_ENTRIES_BYTES + MAX_ITEM_BYTES:
        raise BrokerError.limit_exceeded()
    return value


def without_items(value: Any) -> Any:
    """Copy a result without the items that sensitive-key checks must not inspect."""

    if isinstance(value, Mapping):
        return {key: without_items(item) for key, item in value.items() if key != "items"}
    if isinstance(value, list):
        return [without_items(item) for item in value]
    return value


def _bound(value: Any, limit: int, key: str | None, depth: int) -> Any:
    if isinstance(value, str):
        if len(value) <= limit:
            return value
        if key in _TAIL_KEYS:
            return "…" + value[-(limit - 1) :]
        return value[: limit - 1] + "…"
    if isinstance(value, float) and not math.isfinite(value):
        return None
    if value is None or isinstance(value, (bool, int, float)):
        return value
    if depth >= _MAX_DEPTH:
        return None
    if isinstance(value, list):
        return [_bound(item, limit, key, depth + 1) for item in value[:_MAX_LIST_ITEMS]]
    if isinstance(value, Mapping):
        return {
            str(name): _bound(item, limit, str(name), depth + 1)
            for name, item in list(value.items())[: _MAX_LIST_ITEMS * 2]
        }
    return None


def _is_identifier(value: Any) -> bool:
    return (
        isinstance(value, str)
        and 0 < len(value) <= MAX_ID_LENGTH
        and not any(character.isspace() or ord(character) < 33 for character in value)
    )


def _encoded_size(value: Any) -> int:
    try:
        return len(
            json.dumps(
                value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
            ).encode("utf-8")
        )
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
