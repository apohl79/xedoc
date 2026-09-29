"""Canonical signed frames for the narrow broker-to-broker protocol."""

from __future__ import annotations

from dataclasses import dataclass
import json
import secrets
import socket
import struct
import time
from typing import Any, Mapping
from urllib.parse import urlsplit

from .errors import BrokerError, ErrorCode
from .message_contract import MESSAGE_OPERATION
from .models import Role
from .peer_identity import CertificateMaterial, HostIdentity, sign, verify
from .peer_session_contract import SESSION_OPERATIONS
from .peer_state import Grant, PeerState


PROTOCOL = "xedoc.remote-agent.peer"
PROTOCOL_MAJOR = 1
PROTOCOL_MINOR = 1
PROTOCOL_VERSION = PROTOCOL_MAJOR
MAX_FRAME_BYTES = 4 * 1024 * 1024
FRAME_OVERHEAD_BYTES = 16 * 1024
MAX_REQUEST_SECONDS = 60
MAX_EXCHANGE_SECONDS = 3_605
READ_OPERATIONS = {"host/describe", "workspace/list", "session/list", "session/search"}
PEER_OPERATIONS = READ_OPERATIONS | SESSION_OPERATIONS | {MESSAGE_OPERATION}
CONTROL_KINDS = {"pairing", "grant", "lifecycle"}


@dataclass(frozen=True)
class PeerEndpoint:
    """A normalized broker TLS endpoint, never an app-server endpoint."""

    host: str
    port: int

    @classmethod
    def parse(cls, value: str) -> "PeerEndpoint":
        if (
            not isinstance(value, str)
            or not value
            or len(value.encode("utf-8")) > 2048
            or "\x00" in value
        ):
            raise BrokerError.invalid_request()
        try:
            parsed = urlsplit(value)
            port = parsed.port
        except ValueError as error:
            raise BrokerError.invalid_request() from error
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
        return cls(host=parsed.hostname, port=port)

    @property
    def value(self) -> str:
        host = f"[{self.host}]" if ":" in self.host else self.host
        return f"tls://{host}:{self.port}"


@dataclass(frozen=True)
class PeerTarget:
    """A public endpoint plus its pinned certificate."""

    endpoint: PeerEndpoint
    certificate: CertificateMaterial
    role: Role | None = None


def signed_request(
    identity: HostIdentity,
    role: Role,
    *,
    request_id: str,
    kind: str,
    value: Mapping[str, Any],
    timeout_seconds: float,
) -> dict[str, Any]:
    """Build a bounded, canonical, Ed25519-signed peer request."""

    now = int(time.time())
    frame: dict[str, Any] = {
        "protocol": PROTOCOL,
        "version": PROTOCOL_VERSION,
        "kind": kind,
        "requestId": request_id,
        "from": {"hostId": identity.host_id, "role": role.value},
        "issuedAt": now,
        "expiresAt": now + max(1, min(MAX_REQUEST_SECONDS, int(timeout_seconds) + 1)),
        "nonce": secrets.token_urlsafe(24),
        **value,
    }
    frame["signature"] = sign(identity, canonical(frame))
    return frame


def signed_response(
    identity: HostIdentity,
    role: Role,
    *,
    request_id: str,
    result: Mapping[str, Any],
) -> dict[str, Any]:
    """Build a signed response with no peer-visible filesystem metadata."""

    now = int(time.time())
    frame: dict[str, Any] = {
        "protocol": PROTOCOL,
        "version": PROTOCOL_VERSION,
        "kind": "response",
        "requestId": request_id,
        "from": {"hostId": identity.host_id, "role": role.value},
        "issuedAt": now,
        "expiresAt": now + MAX_REQUEST_SECONDS,
        "result": dict(result),
    }
    frame["signature"] = sign(identity, canonical(frame))
    return frame


def parse_request(
    frame: Mapping[str, Any],
    certificate: CertificateMaterial,
    state: PeerState,
) -> dict[str, Any]:
    """Verify framing, freshness, signature, and nonce before dispatch."""

    if not isinstance(frame, Mapping):
        raise BrokerError.invalid_request()
    kind = frame.get("kind")
    required = {
        "protocol",
        "version",
        "kind",
        "requestId",
        "from",
        "issuedAt",
        "expiresAt",
        "nonce",
        "signature",
    }
    if kind == "operation":
        required |= {"operation", "params"}
    elif kind in CONTROL_KINDS:
        required.add("payload")
    else:
        raise BrokerError.invalid_request()
    if set(frame) != required:
        raise BrokerError.invalid_request()
    if frame["protocol"] != PROTOCOL or frame["version"] != PROTOCOL_VERSION:
        raise BrokerError.invalid_request()
    request_id = identifier(frame["requestId"])
    source = source_identity(frame["from"])
    if source["hostId"] != certificate.host_id:
        raise BrokerError.unauthorized()
    _validate_freshness(frame["issuedAt"], frame["expiresAt"])
    nonce = frame["nonce"]
    if not isinstance(nonce, str) or not 16 <= len(nonce) <= 256:
        raise BrokerError.invalid_request()
    unsigned = dict(frame)
    signature = unsigned.pop("signature")
    verify(certificate, canonical(unsigned), signature)
    state.record_nonce(
        peer_host_id=source["hostId"],
        nonce=nonce,
        expires_at=frame["expiresAt"],
    )
    value = dict(frame)
    value["requestId"] = request_id
    value["from"] = source
    return value


def parse_response(
    frame: Mapping[str, Any],
    request_id: str,
    certificate: CertificateMaterial,
    *,
    expected_role: Role | None,
) -> Mapping[str, Any]:
    """Verify a signed peer response and convert its stable error code."""

    required = {
        "protocol",
        "version",
        "kind",
        "requestId",
        "from",
        "issuedAt",
        "expiresAt",
        "result",
        "signature",
    }
    if (
        not isinstance(frame, Mapping)
        or set(frame) != required
        or frame["protocol"] != PROTOCOL
        or frame["version"] != PROTOCOL_VERSION
        or frame["kind"] != "response"
        or frame["requestId"] != request_id
        or not isinstance(frame["result"], Mapping)
    ):
        raise BrokerError.invalid_request()
    source = source_identity(frame["from"])
    if (
        source["hostId"] != certificate.host_id
        or (expected_role is not None and source["role"] != expected_role.value)
    ):
        raise BrokerError.unauthorized()
    _validate_freshness(frame["issuedAt"], frame["expiresAt"])
    unsigned = dict(frame)
    signature = unsigned.pop("signature")
    verify(certificate, canonical(unsigned), signature)
    result = frame["result"]
    status = result.get("status")
    if status == "ok":
        if set(result) != {"status", "data"} or not isinstance(result["data"], Mapping):
            raise BrokerError.invalid_request()
        return dict(result["data"])
    if (
        status != "error"
        or set(result) != {"status", "error"}
        or not isinstance(result["error"], Mapping)
        or set(result["error"]) != {"code", "retryable"}
    ):
        raise BrokerError.invalid_request()
    try:
        code = ErrorCode(result["error"]["code"])
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    retryable = result["error"]["retryable"]
    if not isinstance(retryable, bool):
        raise BrokerError.invalid_request()
    raise BrokerError(code, retryable=retryable)


def payload(request: Mapping[str, Any], fields: set[str]) -> Mapping[str, Any]:
    value = request.get("payload")
    if not isinstance(value, Mapping) or set(value) != fields:
        raise BrokerError.invalid_request()
    return value


def grants_from_payload(value: Any) -> tuple[Grant, ...]:
    if not isinstance(value, list) or not value or len(value) > 5:
        raise BrokerError.invalid_request()
    grants: list[Grant] = []
    for item in value:
        if (
            not isinstance(item, Mapping)
            or set(item) - {"scope", "workspaceId", "threadId", "expiresAt"}
            or "scope" not in item
        ):
            raise BrokerError.invalid_request()
        scope = item["scope"]
        if scope not in {
            "discovery",
            "workspaceRead",
            "sessionRead",
            "sessionWrite",
            "cancellation",
        }:
            raise BrokerError.invalid_request()
        expires_at = item.get("expiresAt")
        if expires_at is not None and (
            not isinstance(expires_at, int) or isinstance(expires_at, bool)
        ):
            raise BrokerError.invalid_request()
        grants.append(
            Grant(
                scope=scope,
                workspace_id=optional_identifier(item.get("workspaceId")),
                thread_id=optional_identifier(item.get("threadId")),
                expires_at=expires_at,
            )
        )
    return tuple(grants)


def read_frame(connection: socket.socket, limit: int) -> Mapping[str, Any]:
    header = receive_exact(connection, 4)
    size = struct.unpack(">I", header)[0]
    if size <= 0 or size > limit:
        raise BrokerError.limit_exceeded()
    raw = receive_exact(connection, size)
    try:
        value = json.loads(raw.decode("utf-8"), parse_constant=invalid_constant)
    except (UnicodeError, json.JSONDecodeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    if not isinstance(value, Mapping):
        raise BrokerError.invalid_request()
    return value


def write_frame(connection: socket.socket, value: Mapping[str, Any], limit: int) -> None:
    encoded = canonical(value)
    if not encoded or len(encoded) > limit:
        raise BrokerError.limit_exceeded()
    connection.sendall(struct.pack(">I", len(encoded)) + encoded)


def canonical(value: Mapping[str, Any]) -> bytes:
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error


def frame_limit(configured: int) -> int:
    if (
        not isinstance(configured, int)
        or isinstance(configured, bool)
        or configured <= 0
    ):
        raise BrokerError.invalid_request()
    return min(MAX_FRAME_BYTES, configured + FRAME_OVERHEAD_BYTES)


def timeout(value: float) -> float:
    if (
        not isinstance(value, (int, float))
        or isinstance(value, bool)
        or value <= 0
        or value > MAX_EXCHANGE_SECONDS
    ):
        raise BrokerError.invalid_request()
    return float(value)


def exact_fields(
    value: Mapping[str, Any],
    allowed: set[str],
    required: set[str] | None = None,
) -> None:
    required = required or set()
    if set(value) - allowed or not required.issubset(value):
        raise BrokerError.invalid_request()


def identifier(value: Any) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 128
        or any(character.isspace() or ord(character) < 33 for character in value)
    ):
        raise BrokerError.invalid_request()
    return value


def optional_identifier(value: Any) -> str | None:
    if value is None:
        return None
    return identifier(value)


def fingerprint(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def source_identity(value: Any) -> dict[str, str]:
    if not isinstance(value, Mapping) or set(value) != {"hostId", "role"}:
        raise BrokerError.invalid_request()
    host_id = identifier(value["hostId"])
    try:
        role = Role(value["role"])
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    return {"hostId": host_id, "role": role.value}


def _validate_freshness(issued_at: Any, expires_at: Any) -> None:
    now = int(time.time())
    if (
        not isinstance(issued_at, int)
        or isinstance(issued_at, bool)
        or not isinstance(expires_at, int)
        or isinstance(expires_at, bool)
        or expires_at <= issued_at
        or expires_at - issued_at > MAX_REQUEST_SECONDS
        or issued_at > now + 5
        or expires_at <= now
    ):
        raise BrokerError.unauthorized()


def request_id_or_empty(value: Any) -> str:
    try:
        return identifier(value)
    except BrokerError:
        return ""


def receive_exact(connection: socket.socket, size: int) -> bytes:
    chunks: list[bytes] = []
    remaining = size
    while remaining:
        chunk = connection.recv(remaining)
        if not chunk:
            raise BrokerError.unavailable()
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def invalid_constant(_: str) -> None:
    raise ValueError
