"""Typed, bounded data models shared by the local broker modules."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
import ipaddress
from pathlib import Path
from typing import Any, Mapping
from urllib.parse import urlsplit

from .errors import BrokerError


MAX_ID_LENGTH = 128
MAX_TITLE_LENGTH = 256
MAX_SNIPPET_LENGTH = 2048
MAX_CURSOR_LENGTH = 512


class Role(str, Enum):
    """The relationship role configured for a broker host."""

    COORDINATOR = "coordinator"
    MANAGED = "managed"


@dataclass(frozen=True)
class BrokerLimits:
    """Explicit finite limits accepted from host-only configuration."""

    max_attached_sessions: int
    max_message_bytes: int
    max_result_bytes: int
    max_wait_seconds: int
    max_discovery_seconds: int
    audit_retention_days: int

    # These are validation ceilings, not implicit defaults. The values keep a
    # malformed host config from turning into an unbounded broker.
    MAX_ATTACHED_SESSIONS = 1024
    MAX_MESSAGE_BYTES = 1 << 20
    MAX_RESULT_BYTES = 4 << 20
    MAX_WAIT_SECONDS = 3600
    MAX_DISCOVERY_SECONDS = 300
    MAX_AUDIT_RETENTION_DAYS = 3650

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "BrokerLimits":
        if not isinstance(value, Mapping):
            raise BrokerError.invalid_request()
        fields = (
            "max_attached_sessions",
            "max_message_bytes",
            "max_result_bytes",
            "max_wait_seconds",
            "max_discovery_seconds",
            "audit_retention_days",
        )
        parsed: dict[str, int] = {}
        for name in fields:
            raw = value.get(name)
            if raw is None:
                # Accommodate API-facing camelCase while keeping the config
                # source itself snake_case.
                camel = _snake_to_camel(name)
                raw = value.get(camel)
            if not isinstance(raw, int) or isinstance(raw, bool) or raw <= 0:
                raise BrokerError.invalid_request()
            parsed[name] = raw
        limits = cls(**parsed)
        limits.validate()
        return limits

    def validate(self) -> None:
        ceilings = {
            "max_attached_sessions": self.MAX_ATTACHED_SESSIONS,
            "max_message_bytes": self.MAX_MESSAGE_BYTES,
            "max_result_bytes": self.MAX_RESULT_BYTES,
            "max_wait_seconds": self.MAX_WAIT_SECONDS,
            "max_discovery_seconds": self.MAX_DISCOVERY_SECONDS,
            "audit_retention_days": self.MAX_AUDIT_RETENTION_DAYS,
        }
        for name, ceiling in ceilings.items():
            value = getattr(self, name)
            if not isinstance(value, int) or isinstance(value, bool) or not 0 < value <= ceiling:
                raise BrokerError.invalid_request()


@dataclass(frozen=True)
class Workspace:
    """A configured workspace ID and its canonical local root."""

    workspace_id: str
    root: Path

    def __post_init__(self) -> None:
        if not self.workspace_id or len(self.workspace_id) > 64:
            raise BrokerError.invalid_request()
        if not isinstance(self.root, Path) or not self.root.is_absolute():
            raise BrokerError.invalid_request()

    def to_dict(self) -> dict[str, str]:
        return {"workspaceId": self.workspace_id, "root": str(self.root)}


@dataclass(frozen=True)
class PeerListenerConfig:
    """A locally configured TLS listener endpoint for broker peers."""

    endpoint: str
    pairing_endpoint: str | None = None
    advertised_endpoint: str | None = None
    advertised_pairing_endpoint: str | None = None

    def __post_init__(self) -> None:
        _validate_peer_endpoint(self.endpoint)
        if self.pairing_endpoint is not None:
            _validate_peer_endpoint(self.pairing_endpoint)
        advertised = (self.advertised_endpoint, self.advertised_pairing_endpoint)
        if (advertised[0] is None) != (advertised[1] is None):
            raise BrokerError.invalid_request()
        for value in advertised:
            if value is not None:
                _validate_advertised_peer_endpoint(value)


@dataclass(frozen=True)
class StaticPeerConfig:
    """A certificate-pinned peer endpoint from host-only configuration."""

    endpoint: str
    certificate_path: Path

    def __post_init__(self) -> None:
        if (
            not isinstance(self.endpoint, str)
            or not self.endpoint
            or not isinstance(self.certificate_path, Path)
            or not self.certificate_path.is_absolute()
        ):
            raise BrokerError.invalid_request()
        _validate_peer_endpoint(self.endpoint)


@dataclass(frozen=True)
class BrokerConfig:
    """Validated role, workspace allowlist, and finite limits."""

    role: Role
    workspaces: tuple[Workspace, ...]
    limits: BrokerLimits
    peer_listener: PeerListenerConfig | None = None
    static_peers: tuple[StaticPeerConfig, ...] = ()
    managed_coordinator_certificate_paths: tuple[Path, ...] = ()

    def __post_init__(self) -> None:
        self.limits.validate()
        ids = [workspace.workspace_id for workspace in self.workspaces]
        if len(ids) != len(set(ids)):
            raise BrokerError.invalid_request()
        if len(self.static_peers) > 128 or len(
            self.managed_coordinator_certificate_paths
        ) > 128:
            raise BrokerError.invalid_request()
    @property
    def workspace_map(self) -> dict[str, Workspace]:
        return {workspace.workspace_id: workspace for workspace in self.workspaces}

    @classmethod
    def from_mapping(
        cls, value: Mapping[str, Any], workspaces: tuple[Workspace, ...]
    ) -> "BrokerConfig":
        role_value = value.get("role")
        if not isinstance(role_value, str):
            raise BrokerError.invalid_request()
        try:
            role = Role(role_value)
        except ValueError as error:
            raise BrokerError.invalid_request() from error
        limits = BrokerLimits.from_mapping(value.get("limits", {}))
        peer_listener = _peer_listener(value)
        static_peers = _static_peers(value)
        managed_paths = _managed_coordinator_certificate_paths(value)
        return cls(
            role=role,
            workspaces=workspaces,
            limits=limits,
            peer_listener=peer_listener,
            static_peers=static_peers,
            managed_coordinator_certificate_paths=managed_paths,
        )


@dataclass(frozen=True)
class BootstrapDescriptor:
    """The only accepted source of the local app-server controller endpoint."""

    socket_path: Path

    def __post_init__(self) -> None:
        if not isinstance(self.socket_path, Path) or not self.socket_path.is_absolute():
            raise BrokerError.invalid_request()

    @property
    def endpoint(self) -> str:
        return f"unix://{self.socket_path}"


@dataclass(frozen=True)
class SessionSummary:
    """The bounded session projection exposed by the broker."""

    host_id: str
    thread_id: str
    cwd: str
    is_running: bool
    last_activity: int
    summary: Mapping[str, str] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "hostId": self.host_id,
            "threadId": self.thread_id,
            "cwd": self.cwd,
            "isRunning": self.is_running,
            "lastActivity": self.last_activity,
            "summary": dict(self.summary),
        }


@dataclass(frozen=True)
class SearchResult:
    """A bounded search result containing one normalized session."""

    session: SessionSummary
    snippet: str

    def to_dict(self) -> dict[str, Any]:
        return {"thread": self.session.to_dict(), "snippet": self.snippet}


@dataclass(frozen=True)
class SessionPage:
    """A bounded page returned by session list or search."""

    data: tuple[SessionSummary, ...]
    next_cursor: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "data": [session.to_dict() for session in self.data],
            "nextCursor": self.next_cursor,
        }


@dataclass(frozen=True)
class SearchPage:
    """A bounded page returned by session search."""

    data: tuple[SearchResult, ...]
    next_cursor: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "data": [result.to_dict() for result in self.data],
            "nextCursor": self.next_cursor,
        }


class OperationState(str, Enum):
    """States allowed for a local broker operation handle."""

    ACCEPTED = "accepted"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    EXPIRED = "expired"

    @property
    def terminal(self) -> bool:
        return self in {
            OperationState.COMPLETED,
            OperationState.FAILED,
            OperationState.CANCELLED,
            OperationState.EXPIRED,
        }


@dataclass
class OperationRecord:
    """Mutable bounded state associated with one opaque operation handle."""

    operation_id: str
    operation: str
    state: OperationState = OperationState.ACCEPTED
    thread_id: str | None = None
    turn_id: str | None = None
    result: Mapping[str, Any] | None = None
    error: BrokerError | None = None
    created_at: float = 0.0
    updated_at: float = 0.0

    def to_dict(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "operationId": self.operation_id,
            "operation": self.operation,
            "state": self.state.value,
        }
        if self.thread_id is not None:
            value["threadId"] = self.thread_id
        if self.turn_id is not None:
            value["turnId"] = self.turn_id
        if self.result is not None:
            value["result"] = dict(self.result)
        if self.error is not None:
            value["error"] = self.error.to_dict()
        return value


@dataclass(frozen=True)
class OperationHandle:
    """The minimal response returned when an operation is admitted."""

    operation_id: str

    def to_dict(self) -> dict[str, str]:
        return {"operationId": self.operation_id}


def _snake_to_camel(value: str) -> str:
    head, *tail = value.split("_")
    return head + "".join(part[:1].upper() + part[1:] for part in tail)


def _peer_listener(value: Mapping[str, Any]) -> PeerListenerConfig | None:
    raw = value.get("peer_listener", value.get("peerListener"))
    if raw is None:
        return None
    if not isinstance(raw, Mapping):
        raise BrokerError.invalid_request()
    aliases = {
        "pairing_endpoint": "pairingEndpoint",
        "advertised_endpoint": "advertisedEndpoint",
        "advertised_pairing_endpoint": "advertisedPairingEndpoint",
    }
    allowed = {"endpoint", *aliases, *aliases.values()}
    if set(raw) - allowed or "endpoint" not in raw:
        raise BrokerError.invalid_request()
    values: dict[str, Any] = {"endpoint": raw["endpoint"]}
    for snake, camel in aliases.items():
        if snake in raw and camel in raw:
            raise BrokerError.invalid_request()
        values[snake] = raw.get(snake, raw.get(camel))
    return PeerListenerConfig(**values)


def _validate_peer_endpoint(value: object) -> None:
    if (
        not isinstance(value, str)
        or not value
        or len(value.encode("utf-8")) > 2048
        or any(not 0x21 <= ord(character) <= 0x7E for character in value)
    ):
        raise BrokerError.invalid_request()
    try:
        parsed = urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise BrokerError.invalid_request() from error
    if port is not None:
        port_text = parsed.netloc.rsplit(":", 1)[-1]
        if not port_text or port_text[0] not in "123456789" or not port_text.isdigit():
            raise BrokerError.invalid_request()
    if (
        parsed.scheme != "tls"
        or not parsed.hostname
        or port is None
        or not 1 <= port <= 65535
        or parsed.username is not None
        or parsed.password is not None
        or parsed.path not in {"", "/"}
        or parsed.query
        or parsed.fragment
    ):
        raise BrokerError.invalid_request()
    if parsed.netloc.startswith("["):
        try:
            address = ipaddress.IPv6Address(parsed.hostname)
        except ValueError as error:
            raise BrokerError.invalid_request() from error
        if address.scope_id is not None:
            raise BrokerError.invalid_request()


def _validate_advertised_peer_endpoint(value: object) -> None:
    _validate_peer_endpoint(value)
    if not isinstance(value, str):
        raise BrokerError.invalid_request()
    hostname = urlsplit(value).hostname
    if hostname is None:
        raise BrokerError.invalid_request()
    try:
        address = ipaddress.ip_address(hostname)
    except ValueError:
        if "*" in hostname:
            raise BrokerError.invalid_request()
        return
    if address.is_unspecified or address.is_loopback:
        raise BrokerError.invalid_request()


def _static_peers(value: Mapping[str, Any]) -> tuple[StaticPeerConfig, ...]:
    raw = value.get("static_peers", value.get("staticPeers", []))
    if not isinstance(raw, list) or len(raw) > 128:
        raise BrokerError.invalid_request()
    peers: list[StaticPeerConfig] = []
    for item in raw:
        if not isinstance(item, Mapping):
            raise BrokerError.invalid_request()
        keys = set(item)
        if keys != {"endpoint", "certificate_path"} and keys != {
            "endpoint",
            "certificatePath",
        }:
            raise BrokerError.invalid_request()
        certificate_path = item.get("certificate_path", item.get("certificatePath"))
        if not isinstance(certificate_path, str):
            raise BrokerError.invalid_request()
        peers.append(StaticPeerConfig(item.get("endpoint"), Path(certificate_path)))
    return tuple(peers)


def _managed_coordinator_certificate_paths(
    value: Mapping[str, Any],
) -> tuple[Path, ...]:
    raw = value.get(
        "managed_coordinator_certificate_paths",
        value.get("managedCoordinatorCertificatePaths", []),
    )
    if not isinstance(raw, list) or len(raw) > 128:
        raise BrokerError.invalid_request()
    if not all(isinstance(path, str) for path in raw):
        raise BrokerError.invalid_request()
    return tuple(Path(path) for path in raw)
