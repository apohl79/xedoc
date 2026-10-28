"""Bounded LAN discovery for Xedoc Host GUI instances."""

import ipaddress
import json
import logging
import secrets
import socket
import threading
import time
from dataclasses import dataclass
from urllib.parse import urlsplit

from .lan_interfaces import ipv4_broadcasts

logger = logging.getLogger(__name__)
DISCOVERY_PROTOCOL = "xedoc.host-gui.discovery"
DISCOVERY_VERSION = 1
DISCOVERY_GROUP = "239.255.70.42"
DISCOVERY_PORT = 43_391
MAX_PACKET_BYTES = 2_048
MAX_CANDIDATES = 64
MAX_HOSTNAME_BYTES = 255


@dataclass(frozen=True)
class HostGuiCandidate:
    hostname: str
    origin: str


class HostGuiDiscovery:
    def __init__(self, hostname: str, origin: str) -> None:
        self.hostname = hostname
        self.origin = origin
        self._socket: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stopping = threading.Event()

    def start(self) -> bool:
        if self._socket is not None:
            return True
        if not _hostname(self.hostname) or not _advertisable_origin(self.origin):
            return False
        listener: socket.socket | None = None
        try:
            listener = socket.socket(
                socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP
            )
            listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            listener.bind(("", DISCOVERY_PORT))
            membership = socket.inet_aton(DISCOVERY_GROUP) + socket.inet_aton("0.0.0.0")
            try:
                listener.setsockopt(
                    socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, membership
                )
            except OSError:
                logger.warning("Host GUI multicast unavailable; using LAN broadcast")
            listener.settimeout(0.25)
        except OSError:
            if listener is not None:
                listener.close()
            return False
        self._socket = listener
        self._stopping.clear()
        self._thread = threading.Thread(
            target=self._serve,
            name="xedoc-host-gui-discovery",
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
            if query_id is None:
                continue
            response = _encode(
                {
                    "protocol": DISCOVERY_PROTOCOL,
                    "version": DISCOVERY_VERSION,
                    "kind": "response",
                    "queryId": query_id,
                    "hostname": self.hostname,
                    "origin": self.origin,
                }
            )
            try:
                listener.sendto(response, address)
            except OSError:
                continue


def discover_host_guis(
    timeout_seconds: float = 1.5, current_origin: str | None = None
) -> tuple[HostGuiCandidate, ...]:
    if (
        not isinstance(timeout_seconds, (int, float))
        or isinstance(timeout_seconds, bool)
        or not 0 < timeout_seconds <= 5
    ):
        raise ValueError("Host GUI discovery timeout must be between 0 and 5 seconds")
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
    candidates: dict[tuple[str, str], HostGuiCandidate] = {}
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM, socket.IPPROTO_UDP) as client:
        client.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_TTL, 1)
        client.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_LOOP, 1)
        client.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        destinations = {DISCOVERY_GROUP, "255.255.255.255", "127.0.0.1"}
        try:
            destinations.update(ipv4_broadcasts())
        except OSError:
            logger.exception("Cannot enumerate Host GUI discovery interfaces")
        sent = False
        for destination in sorted(destinations):
            try:
                client.sendto(query, (destination, DISCOVERY_PORT))
                sent = True
            except OSError:
                logger.debug(
                    "Host GUI discovery destination unavailable: %s", destination
                )
        if not sent:
            raise OSError("No network interface available for Host GUI discovery")
        while len(candidates) < MAX_CANDIDATES:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            client.settimeout(min(0.25, remaining))
            try:
                payload, _address = client.recvfrom(MAX_PACKET_BYTES + 1)
            except TimeoutError:
                continue
            except ConnectionResetError:
                continue
            candidate = _candidate(payload, query_id)
            if candidate is None or (
                current_origin is not None
                and candidate.origin.casefold() == current_origin.casefold()
            ):
                continue
            candidates[(candidate.hostname.casefold(), candidate.origin.casefold())] = (
                candidate
            )
    return tuple(
        sorted(
            candidates.values(),
            key=lambda item: (item.hostname.casefold(), item.origin),
        )
    )


def _candidate(payload: bytes, query_id: str) -> HostGuiCandidate | None:
    value = _decode(payload)
    if (
        value is None
        or set(value)
        != {"protocol", "version", "kind", "queryId", "hostname", "origin"}
        or value["protocol"] != DISCOVERY_PROTOCOL
        or value["version"] != DISCOVERY_VERSION
        or value["kind"] != "response"
        or value["queryId"] != query_id
        or not _hostname(value["hostname"])
        or not isinstance(value["origin"], str)
        or not _advertisable_origin(value["origin"])
    ):
        return None
    return HostGuiCandidate(value["hostname"], value["origin"])


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
    if (
        not isinstance(query_id, str)
        or not 16 <= len(query_id) <= 128
        or not query_id.isascii()
        or not all(character.isalnum() or character in "-_" for character in query_id)
    ):
        return None
    return query_id


def _decode(payload: bytes) -> dict | None:
    if not payload or len(payload) > MAX_PACKET_BYTES:
        return None
    try:
        value = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError):
        return None
    return value if isinstance(value, dict) else None


def _encode(value: dict[str, object]) -> bytes:
    payload = json.dumps(
        value, ensure_ascii=False, separators=(",", ":"), allow_nan=False
    ).encode("utf-8")
    if not payload or len(payload) > MAX_PACKET_BYTES:
        raise ValueError("Host GUI discovery message exceeds size limit")
    return payload


def _hostname(value: object) -> bool:
    return (
        isinstance(value, str)
        and 1 <= len(value) <= MAX_HOSTNAME_BYTES
        and value.isascii()
        and all(character.isalnum() or character in "._-" for character in value)
    )


def _advertisable_origin(value: str) -> bool:
    if (
        not isinstance(value, str)
        or len(value) > 512
        or not value.isascii()
        or any(not 0x21 <= ord(character) <= 0x7E for character in value)
    ):
        return False
    try:
        origin = urlsplit(value)
        port = origin.port
    except ValueError:
        return False
    hostname = origin.hostname
    if (
        origin.scheme != "https"
        or not hostname
        or not origin.netloc
        or origin.username is not None
        or origin.password is not None
        or origin.path
        or origin.query
        or origin.fragment
        or "?" in value
        or "#" in value
        or port == 0
    ):
        return False
    if hostname.casefold() == "localhost" or hostname.casefold().endswith(".localhost"):
        return False
    try:
        if ipaddress.ip_address(hostname).is_loopback:
            return False
    except ValueError:
        pass
    return True
