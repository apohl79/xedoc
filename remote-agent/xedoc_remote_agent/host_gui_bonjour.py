"""Bonjour hints for secure Host GUIs, without changing their HTTPS identity."""

import ipaddress
import logging
import secrets
import threading
import time
from urllib.parse import urlsplit

from .host_gui_discovery import _advertisable_origin


logger = logging.getLogger(__name__)
SERVICE_TYPE = "_xedoc-gui._tcp.local."


def _addresses() -> tuple[bytes, ...]:
    from ifaddr import get_adapters

    addresses = set()
    for adapter in get_adapters():
        for entry in adapter.ips:
            address = ipaddress.ip_address(
                entry.ip[0] if isinstance(entry.ip, tuple) else entry.ip
            )
            if not address.is_loopback and not address.is_unspecified:
                addresses.add(address.packed)
    return tuple(sorted(addresses))


class HostGuiBonjour:
    def __init__(self, hostname: str, origin: str) -> None:
        self.hostname = hostname
        self.origin = origin
        self._stopping = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        properties = {"origin": self.origin, "version": "1", "hostname": self.hostname}
        if not _advertisable_origin(self.origin) or any(
            len(f"{key}={value}".encode()) > 255 for key, value in properties.items()
        ):
            logger.warning(
                "Host GUI Bonjour unavailable: invalid or oversized HTTPS identity"
            )
            return
        if self._thread is not None:
            return
        self._stopping.clear()
        self._thread = threading.Thread(
            target=self._serve,
            args=(properties,),
            name="xedoc-host-gui-bonjour",
            daemon=True,
        )
        try:
            self._thread.start()
        except RuntimeError:
            self._thread = None
            logger.warning("Host GUI Bonjour could not start", exc_info=True)

    def stop(self) -> None:
        self._stopping.set()
        thread = self._thread
        if thread is not None:
            thread.join(timeout=10)
            if thread.is_alive():
                logger.warning(
                    "Host GUI Bonjour cleanup did not finish within 10 seconds"
                )
            else:
                self._thread = None

    def _serve(self, properties: dict[str, str]) -> None:
        try:
            from zeroconf import IPVersion, ServiceInfo, Zeroconf
        except ImportError:
            logger.warning(
                "Host GUI Bonjour unavailable: install the locked remote-agent dependencies"
            )
            return
        port = urlsplit(self.origin).port or 443
        name = f"{self.hostname[:48]}-{port}.{SERVICE_TYPE}"
        server = f"xedoc-{secrets.token_hex(8)}.local."
        warned = False
        while not self._stopping.is_set():
            try:
                addresses = _addresses()
                if not addresses:
                    raise OSError("No active LAN address")
                with Zeroconf(ip_version=IPVersion.All) as announcement:
                    info = ServiceInfo(
                        SERVICE_TYPE,
                        name,
                        addresses=list(addresses),
                        port=port,
                        properties=properties,
                        server=server,
                    )
                    announcement.register_service(info, allow_name_change=True)
                    warned = False
                    deadline = time.monotonic() + 30
                    while not self._stopping.wait(5):
                        if _addresses() != addresses or time.monotonic() >= deadline:
                            break
            except Exception:
                if not warned:
                    logger.warning(
                        "Host GUI Bonjour announcement failed; retrying", exc_info=True
                    )
                    warned = True
                self._stopping.wait(5)
