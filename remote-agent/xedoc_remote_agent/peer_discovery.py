"""Bounded LAN discovery for unpaired remote-agent hosts."""

from __future__ import annotations

from dataclasses import dataclass
import ipaddress
import json
import os
from pathlib import Path
import secrets
import socket
import tempfile
import threading
import time
from typing import Callable, Iterable, Mapping

from .errors import BrokerError
from .models import Role
from .peer_protocol import PROTOCOL_MAJOR, PROTOCOL_MINOR, PeerEndpoint, fingerprint


DISCOVERY_PROTOCOL = "xedoc.remote-agent.discovery"
DISCOVERY_VERSION = 1
DISCOVERY_GROUP = "239.255.70.40"
DISCOVERY_PORT = 43_371
DISCOVERY_PORT_SLOTS = 16
DISCOVERY_TOKEN_TTL_SECONDS = 60
MAX_DISCOVERY_PACKET_BYTES = 8 * 1024
MAX_DISCOVERY_RESULTS = 128
MAX_DISCOVERY_TOKENS = 256
MAX_DIRECT_DESTINATIONS = 16
_LOCAL_DISCOVERY_DIRECTORY = (
    Path(tempfile.gettempdir()) / f"xedoc-remote-agent-discovery-{os.getuid()}"
)


@dataclass(frozen=True)
class DiscoveryCandidate:
    """An ephemeral untrusted LAN discovery result."""

    host_id: str
    role: Role
    endpoint: PeerEndpoint
    pairing_endpoint: PeerEndpoint
    fingerprint: str
    protocol_major: int
    protocol_minor: int
    capabilities: frozenset[str]
    pairing_token: str


class PeerDiscovery:
    """Answer bounded multicast queries and issue one-time pairing capabilities."""

    def __init__(
        self,
        *,
        host_id: str,
        role: Role,
        fingerprint_value: str,
        endpoints: Callable[[str], tuple[PeerEndpoint, PeerEndpoint] | None],
        capabilities: Callable[[], frozenset[str]],
    ) -> None:
        if not isinstance(host_id, str) or not host_id or not isinstance(role, Role):
            raise BrokerError.invalid_request()
        if not fingerprint(fingerprint_value):
            raise BrokerError.invalid_request()
        self._host_id = host_id
        self._role = role
        self._fingerprint = fingerprint_value
        self._endpoints = endpoints
        self._capabilities = capabilities
        self._tokens: dict[str, float] = {}
        self._tokens_lock = threading.Lock()
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stopping = threading.Event()
        self._local_registration: Path | None = None

    def start(self) -> None:
        if self._socket is not None:
            raise BrokerError.conflict()
        try:
            listener = _bind_listener()
        except OSError as error:
            raise BrokerError.unavailable() from error
        self._socket = listener
        self._register_local_listener(listener)
        self._stopping.clear()
        self._thread = threading.Thread(
            target=self._serve,
            name="xedoc-remote-agent-discovery",
            daemon=True,
        )
        self._thread.start()

    def stop(self) -> None:
        self._stopping.set()
        listener, self._socket = self._socket, None
        self._remove_local_listener()
        if listener is not None:
            try:
                listener.close()
            except OSError:
                pass
        thread, self._thread = self._thread, None
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=1.0)

    def discover(
        self, timeout_seconds: int, destinations: Iterable[str] = ()
    ) -> tuple[DiscoveryCandidate, ...]:
        if (
            not isinstance(timeout_seconds, int)
            or isinstance(timeout_seconds, bool)
            or timeout_seconds < 0
        ):
            raise BrokerError.invalid_request()
        direct_destinations = _direct_destinations(destinations)
        if timeout_seconds == 0:
            return ()
        query_id = f"discovery_{secrets.token_urlsafe(18)}"
        query = _encode(
            {
                "protocol": DISCOVERY_PROTOCOL,
                "version": DISCOVERY_VERSION,
                "kind": "query",
                "queryId": query_id,
            }
        )
        deadline = time.monotonic() + timeout_seconds
        results: dict[str, DiscoveryCandidate] = {}
        try:
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP) as client:
                client.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
                client.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
                client.settimeout(min(0.2, timeout_seconds))
                destinations = {
                    *((DISCOVERY_GROUP, port) for port in _discovery_ports()),
                    *(("127.0.0.1", port) for port in self._local_discovery_ports()),
                    *direct_destinations,
                }
                sent = False
                for destination in destinations:
                    try:
                        client.sendto(query, destination)
                    except OSError:
                        continue
                    sent = True
                if not sent:
                    raise BrokerError.unavailable()
                while len(results) < MAX_DISCOVERY_RESULTS:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        break
                    client.settimeout(min(0.2, remaining))
                    try:
                        payload, address = client.recvfrom(MAX_DISCOVERY_PACKET_BYTES + 1)
                    except TimeoutError:
                        continue
                    candidate = _candidate(payload, address, query_id)
                    if candidate is not None and candidate.host_id != self._host_id:
                        results[candidate.host_id] = candidate
        except OSError as error:
            raise BrokerError.unavailable() from error
        return tuple(sorted(results.values(), key=lambda candidate: candidate.host_id))

    def consume_pairing_token(self, token: str) -> bool:
        if not isinstance(token, str) or not 16 <= len(token) <= 256:
            return False
        now = time.monotonic()
        with self._tokens_lock:
            self._prune_tokens(now)
            expires_at = self._tokens.pop(token, None)
        return expires_at is not None and expires_at > now

    def has_pairing_token(self, token: str) -> bool:
        """Check an advertised token before parsing an unauthenticated frame."""

        if not isinstance(token, str) or not 16 <= len(token) <= 256:
            return False
        now = time.monotonic()
        with self._tokens_lock:
            self._prune_tokens(now)
            expires_at = self._tokens.get(token)
        return expires_at is not None and expires_at > now

    def _serve(self) -> None:
        while not self._stopping.is_set():
            listener = self._socket
            if listener is None:
                return
            try:
                payload, address = listener.recvfrom(MAX_DISCOVERY_PACKET_BYTES + 1)
            except TimeoutError:
                continue
            except OSError:
                if self._stopping.is_set():
                    return
                continue
            query_id = _query_id(payload)
            endpoints = self._endpoints(address[0])
            if query_id is None or endpoints is None:
                continue
            endpoint, pairing_endpoint = endpoints
            token = self._issue_pairing_token()
            if token is None:
                continue
            try:
                response = _encode(
                    {
                        "protocol": DISCOVERY_PROTOCOL,
                        "version": DISCOVERY_VERSION,
                        "kind": "response",
                        "queryId": query_id,
                        "hostId": self._host_id,
                        "role": self._role.value,
                        "endpoint": endpoint.value,
                        "pairingEndpoint": pairing_endpoint.value,
                        "fingerprint": self._fingerprint,
                        "protocolMajor": PROTOCOL_MAJOR,
                        "protocolMinor": PROTOCOL_MINOR,
                        "capabilities": sorted(self._capabilities()),
                        "pairingToken": token,
                    }
                )
                listener.sendto(response, address)
            except OSError:
                continue

    def _issue_pairing_token(self) -> str | None:
        token = secrets.token_urlsafe(24)
        now = time.monotonic()
        with self._tokens_lock:
            self._prune_tokens(now)
            if len(self._tokens) >= MAX_DISCOVERY_TOKENS:
                return None
            self._tokens[token] = now + DISCOVERY_TOKEN_TTL_SECONDS
        return token

    def _prune_tokens(self, now: float) -> None:
        for token, expires_at in tuple(self._tokens.items()):
            if expires_at <= now:
                del self._tokens[token]

    def _register_local_listener(self, listener: socket.socket) -> None:
        try:
            _LOCAL_DISCOVERY_DIRECTORY.mkdir(mode=0o700, parents=True, exist_ok=True)
            _LOCAL_DISCOVERY_DIRECTORY.chmod(0o700)
            port = int(listener.getsockname()[1])
            registration = _LOCAL_DISCOVERY_DIRECTORY / f"{self._host_id}.json"
            temporary = registration.with_suffix(".tmp")
            temporary.write_text(
                json.dumps(
                    {"hostId": self._host_id, "port": port},
                    separators=(",", ":"),
                ),
                encoding="utf-8",
            )
            temporary.chmod(0o600)
            temporary.replace(registration)
            self._local_registration = registration
        except OSError:
            self._local_registration = None

    def _remove_local_listener(self) -> None:
        registration, self._local_registration = self._local_registration, None
        if registration is None:
            return
        try:
            registration.unlink(missing_ok=True)
        except OSError:
            pass

    def _local_discovery_ports(self) -> tuple[int, ...]:
        try:
            registrations = tuple(_LOCAL_DISCOVERY_DIRECTORY.glob("*.json"))
        except OSError:
            return ()
        ports: set[int] = set()
        for registration in registrations[:MAX_DISCOVERY_RESULTS]:
            try:
                value = json.loads(registration.read_text(encoding="utf-8"))
            except (OSError, UnicodeError, json.JSONDecodeError):
                continue
            if (
                not isinstance(value, Mapping)
                or set(value) != {"hostId", "port"}
                or not isinstance(value["hostId"], str)
                or not isinstance(value["port"], int)
                or isinstance(value["port"], bool)
                or not 1 <= value["port"] <= 65535
            ):
                continue
            ports.add(value["port"])
        return tuple(sorted(ports))


def _encode(value: Mapping[str, object]) -> bytes:
    try:
        payload = json.dumps(
            value,
            ensure_ascii=False,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    if not payload or len(payload) > MAX_DISCOVERY_PACKET_BYTES:
        raise BrokerError.limit_exceeded()
    return payload


def _bind_listener() -> socket.socket:
    last_error: OSError | None = None
    for port in _discovery_ports():
        listener: socket.socket | None = None
        try:
            listener = socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP)
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("", port))
            membership = socket.inet_aton(DISCOVERY_GROUP) + socket.inet_aton("0.0.0.0")
            listener.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, membership)
            listener.settimeout(0.2)
            return listener
        except OSError as error:
            last_error = error
            if listener is not None:
                listener.close()
    if last_error is None:
        raise BrokerError.unavailable()
    raise last_error


def _discovery_ports() -> range:
    return range(DISCOVERY_PORT, DISCOVERY_PORT + DISCOVERY_PORT_SLOTS)


def _query_id(payload: bytes) -> str | None:
    value = _decode(payload)
    if (
        value is None
        or set(value) != {"protocol", "version", "kind", "queryId"}
        or value["protocol"] != DISCOVERY_PROTOCOL
        or value["version"] != DISCOVERY_VERSION
        or value["kind"] != "query"
    ):
        return None
    query_id = value["queryId"]
    if not isinstance(query_id, str) or not 16 <= len(query_id) <= 256:
        return None
    return query_id


def _candidate(
    payload: bytes, address: tuple[str, int], query_id: str
) -> DiscoveryCandidate | None:
    value = _decode(payload)
    required = {
        "protocol",
        "version",
        "kind",
        "queryId",
        "hostId",
        "role",
        "endpoint",
        "pairingEndpoint",
        "fingerprint",
        "protocolMajor",
        "protocolMinor",
        "capabilities",
        "pairingToken",
    }
    if (
        value is None
        or set(value) != required
        or value["protocol"] != DISCOVERY_PROTOCOL
        or value["version"] != DISCOVERY_VERSION
        or value["kind"] != "response"
        or value["queryId"] != query_id
    ):
        return None
    host_id = value["hostId"]
    role = value["role"]
    endpoint = value["endpoint"]
    pairing_endpoint = value["pairingEndpoint"]
    protocol_major = value["protocolMajor"]
    protocol_minor = value["protocolMinor"]
    capabilities = value["capabilities"]
    pairing_token = value["pairingToken"]
    if (
        not isinstance(host_id, str)
        or not 1 <= len(host_id) <= 128
        or not isinstance(role, str)
        or not isinstance(endpoint, str)
        or not endpoint.isascii()
        or not isinstance(pairing_endpoint, str)
        or not pairing_endpoint.isascii()
        or not fingerprint(value["fingerprint"])
        or not isinstance(protocol_major, int)
        or isinstance(protocol_major, bool)
        or protocol_major != PROTOCOL_MAJOR
        or not isinstance(protocol_minor, int)
        or isinstance(protocol_minor, bool)
        or protocol_minor < 0
        or not isinstance(capabilities, list)
        or len(capabilities) > 32
        or not all(
            isinstance(capability, str) and 1 <= len(capability) <= 64
            for capability in capabilities
        )
        or not isinstance(pairing_token, str)
        or not 16 <= len(pairing_token) <= 256
    ):
        return None
    try:
        peer_role = Role(role)
        endpoint = PeerEndpoint.parse(endpoint)
        pairing_endpoint = PeerEndpoint.parse(pairing_endpoint)
    except (BrokerError, ValueError):
        return None
    return DiscoveryCandidate(
        host_id=host_id,
        role=peer_role,
        endpoint=endpoint,
        pairing_endpoint=pairing_endpoint,
        fingerprint=value["fingerprint"],
        protocol_major=protocol_major,
        protocol_minor=protocol_minor,
        capabilities=frozenset(capabilities),
        pairing_token=pairing_token,
    )


def _decode(payload: bytes) -> Mapping[str, object] | None:
    if not payload or len(payload) > MAX_DISCOVERY_PACKET_BYTES:
        return None
    try:
        value = json.loads(payload.decode("utf-8"), parse_constant=_invalid_constant)
    except (UnicodeError, ValueError, json.JSONDecodeError):
        return None
    return value if isinstance(value, Mapping) else None


def _invalid_constant(_: str) -> None:
    raise ValueError


def _direct_destinations(values: Iterable[str]) -> tuple[tuple[str, int], ...]:
    if isinstance(values, (str, bytes)):
        raise BrokerError.invalid_request()
    try:
        supplied = tuple(values)
    except TypeError as error:
        raise BrokerError.invalid_request() from error
    if len(supplied) > MAX_DIRECT_DESTINATIONS:
        raise BrokerError.limit_exceeded()
    parsed = {_direct_destination(value) for value in supplied}
    return tuple(sorted(parsed))


def _direct_destination(value: object) -> tuple[str, int]:
    if (
        not isinstance(value, str)
        or not value.isascii()
        or not 1 <= len(value) <= 255
    ):
        raise BrokerError.invalid_request()
    host, separator, raw_port = value.rpartition(":")
    if not separator or not host or ":" in host or not raw_port.isdecimal():
        raise BrokerError.invalid_request()
    if not 1 <= len(raw_port) <= 5 or not 1 <= int(raw_port) <= 65535:
        raise BrokerError.invalid_request()
    try:
        address = ipaddress.IPv4Address(host)
    except ipaddress.AddressValueError as error:
        raise BrokerError.invalid_request() from error
    if str(address) != host:
        raise BrokerError.invalid_request()
    return str(address), int(raw_port)
