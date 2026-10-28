"""Bounded LAN discovery for Host GUI certificate authorities."""

from dataclasses import dataclass
import hashlib
import ipaddress
import json
import secrets
import socket
import threading
import time
from urllib.parse import urlsplit


DISCOVERY_PROTOCOL = "xedoc.host-gui.ca-discovery"
DISCOVERY_VERSION = 1
DISCOVERY_GROUP = "239.255.70.41"
DISCOVERY_PORT = 43_390
MAX_PACKET_BYTES = 4 * 1024
MAX_CANDIDATES = 32
MAX_PENDING_CHALLENGES = 128
CHALLENGE_TTL_SECONDS = 300


@dataclass(frozen=True)
class SharedCACandidate:
    hostname: str
    origin: str
    fingerprint: str
    challenge: str
    client_address: str


class HostCADiscovery:
    def __init__(self, hostname: str, origin: str, fingerprint: str) -> None:
        self.hostname = hostname
        self.origin = origin
        self.fingerprint = fingerprint
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stopping = threading.Event()
        self._challenge_lock = threading.Lock()
        self._challenges: dict[str, tuple[str, float]] = {}

    def start(self) -> bool:
        if self._socket is not None:
            return True
        listener: socket.socket | None = None
        try:
            listener = socket.socket(
                socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP
            )
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("", DISCOVERY_PORT))
            membership = socket.inet_aton(DISCOVERY_GROUP) + socket.inet_aton("0.0.0.0")
            listener.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, membership)
            listener.settimeout(0.25)
        except OSError:
            if listener is not None:
                listener.close()
            return False
        self._socket = listener
        self._stopping.clear()
        self._thread = threading.Thread(
            target=self._serve,
            name="xedoc-host-gui-ca-discovery",
            daemon=True,
        )
        self._thread.start()
        return True

    def stop(self) -> None:
        self._stopping.set()
        listener, self._socket = self._socket, None
        if listener is not None:
            try:
                listener.close()
            except OSError:
                pass
        thread, self._thread = self._thread, None
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=1)

    def consume_challenge(self, challenge: object, source_address: str) -> bool:
        if not isinstance(challenge, str) or not 32 <= len(challenge) <= 128:
            return False
        address = _normalized_address(source_address)
        if address is None:
            return False
        digest = hashlib.sha256(challenge.encode()).hexdigest()
        with self._challenge_lock:
            self._remove_expired_challenges()
            value = self._challenges.get(digest)
            if value is None or value[0] != address:
                return False
            del self._challenges[digest]
            return True

    def _serve(self) -> None:
        while not self._stopping.is_set():
            listener = self._socket
            if listener is None:
                return
            try:
                payload, address = listener.recvfrom(MAX_PACKET_BYTES + 1)
            except TimeoutError:
                continue
            except OSError:
                if self._stopping.is_set():
                    return
                continue
            query_id = _query_id(payload)
            requester = _normalized_address(address[0])
            if query_id is None or requester is None:
                continue
            challenge = self._issue_challenge(requester)
            if challenge is None:
                continue
            response = _encode(
                {
                    "protocol": DISCOVERY_PROTOCOL,
                    "version": DISCOVERY_VERSION,
                    "kind": "response",
                    "queryId": query_id,
                    "hostname": self.hostname,
                    "origin": self.origin,
                    "fingerprint": self.fingerprint,
                    "challenge": challenge,
                }
            )
            try:
                listener.sendto(response, address)
            except OSError:
                continue

    def _issue_challenge(self, source_address: str) -> str | None:
        with self._challenge_lock:
            self._remove_expired_challenges()
            if len(self._challenges) >= MAX_PENDING_CHALLENGES:
                return None
            challenge = secrets.token_urlsafe(32)
            digest = hashlib.sha256(challenge.encode()).hexdigest()
            self._challenges[digest] = (
                source_address,
                time.monotonic() + CHALLENGE_TTL_SECONDS,
            )
            return challenge

    def _remove_expired_challenges(self) -> None:
        now = time.monotonic()
        self._challenges = {
            digest: value
            for digest, value in self._challenges.items()
            if value[1] > now
        }


def discover_shared_cas(timeout_seconds: float = 3.0) -> tuple[SharedCACandidate, ...]:
    if (
        not isinstance(timeout_seconds, (int, float))
        or isinstance(timeout_seconds, bool)
        or not 0 < timeout_seconds <= 15
    ):
        raise ValueError("CA discovery timeout must be between 0 and 15 seconds")
    query_id = secrets.token_urlsafe(18)
    query = _encode(
        {
            "protocol": DISCOVERY_PROTOCOL,
            "version": DISCOVERY_VERSION,
            "kind": "query",
            "queryId": query_id,
        }
    )
    deadline = time.monotonic() + timeout_seconds
    candidates: dict[tuple[str, str], SharedCACandidate] = {}
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP) as client:
        client.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
        client.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
        client.sendto(query, (DISCOVERY_GROUP, DISCOVERY_PORT))
        while len(candidates) < MAX_CANDIDATES:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            client.settimeout(min(0.25, remaining))
            try:
                payload, responder = client.recvfrom(MAX_PACKET_BYTES + 1)
            except TimeoutError:
                continue
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as route:
                try:
                    route.connect((responder[0], 1))
                except OSError:
                    continue
                peer_address = _normalized_address(route.getsockname()[0])
            if peer_address is None:
                continue
            candidate = _candidate(payload, query_id, peer_address)
            if candidate is not None:
                candidates[(candidate.origin, candidate.fingerprint)] = candidate
    return tuple(
        sorted(
            candidates.values(),
            key=lambda candidate: (candidate.hostname, candidate.origin),
        )
    )


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
    if not isinstance(query_id, str) or not 16 <= len(query_id) <= 128:
        return None
    return query_id


def _candidate(
    payload: bytes, query_id: str, client_address: str
) -> SharedCACandidate | None:
    value = _decode(payload)
    required = {
        "protocol",
        "version",
        "kind",
        "queryId",
        "hostname",
        "origin",
        "fingerprint",
        "challenge",
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
    hostname = value["hostname"]
    origin = value["origin"]
    fingerprint = value["fingerprint"]
    challenge = value["challenge"]
    if (
        not isinstance(hostname, str)
        or not 1 <= len(hostname) <= 255
        or not hostname.isascii()
        or any(
            not (character.isalnum() or character in "._-") for character in hostname
        )
        or not isinstance(origin, str)
        or not _is_https_origin(origin)
        or not isinstance(fingerprint, str)
        or not _is_fingerprint(fingerprint)
        or not isinstance(challenge, str)
        or not 32 <= len(challenge) <= 128
        or _normalized_address(client_address) is None
    ):
        return None
    origin_parts = urlsplit(origin)
    try:
        socket.getaddrinfo(
            origin_parts.hostname,
            origin_parts.port or 443,
            family=socket.AF_INET,
            type=socket.SOCK_STREAM,
        )
    except OSError:
        return None
    return SharedCACandidate(
        hostname=hostname,
        origin=origin,
        fingerprint=fingerprint,
        challenge=challenge,
        client_address=_normalized_address(client_address) or client_address,
    )


def _decode(payload: bytes) -> dict | None:
    if not payload or len(payload) > MAX_PACKET_BYTES:
        return None
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return None
    return value if isinstance(value, dict) else None


def _encode(value: dict[str, object]) -> bytes:
    payload = json.dumps(
        value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")
    if not payload or len(payload) > MAX_PACKET_BYTES:
        raise ValueError("CA discovery message exceeds size limit")
    return payload


def _is_https_origin(value: str) -> bool:
    if not value.isascii() or any(
        not 0x21 <= ord(character) <= 0x7E for character in value
    ):
        return False
    try:
        origin = urlsplit(value)
        origin.port
    except ValueError:
        return False
    return (
        origin.scheme == "https"
        and bool(origin.hostname)
        and not origin.username
        and not origin.password
        and not origin.path
        and not origin.query
        and not origin.fragment
    )


def _is_fingerprint(value: str) -> bool:
    compact = value.replace(":", "")
    return (
        len(value) == 95
        and len(compact) == 64
        and all(character in "0123456789abcdefABCDEF" for character in compact)
    )


def _normalized_address(value: str) -> str | None:
    try:
        address = ipaddress.ip_address(value.split("%", 1)[0])
    except (AttributeError, ValueError):
        return None
    if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped:
        address = address.ipv4_mapped
    if address.is_unspecified:
        return None
    return str(address)
