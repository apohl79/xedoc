"""Device-protected, end-to-end TLS listener for an untrusted TCP relay."""

from http.server import ThreadingHTTPServer
import logging
from pathlib import Path
import threading

from .host_device_pairing import HostDevicePairing
from .host_gui_config import HostGuiConfig


def load_bridge_config(home: Path) -> HostGuiConfig | None:
    bridge_home = home / "bridge"
    if not (bridge_home / "host-gui" / "server.json").exists():
        return None
    config = HostGuiConfig.load(bridge_home)
    if (
        config.listen_address != "127.0.0.1"
        or not config.origin.startswith("https://")
        or config.tls_client_ca is None
        or config.tls_client_ca.resolve()
        != (home / "host-gui" / "device-pairing" / "ca.pem").resolve()
    ):
        raise ValueError("The bridge requires loopback HTTPS and app-managed device certificates")
    return config


class BridgeHostServer(ThreadingHTTPServer):
    daemon_threads = True
    secure = True
    bridge_only = True

    def __init__(self, host_server, home: Path, config: HostGuiConfig):
        self.pairing_server = host_server
        self.origin = config.origin
        self.authority = config.origin.removeprefix("https://")
        self.tls_config = config
        self.tls_context = config.create_tls_context()
        self.tls_certificate = config.tls_cert.read_bytes()
        self.device_pairing = HostDevicePairing(home)
        self.slots = threading.BoundedSemaphore(48)
        super().__init__((config.listen_address, config.port), host_server.RequestHandlerClass)

    def __getattr__(self, name):
        return getattr(self.pairing_server, name)

    def origin_for_authority(self, authority):
        return self.origin

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            request.settimeout(15)
            request = self.tls_context.wrap_socket(request, server_side=True)
            super().process_request_thread(request, client_address)
        except OSError:
            logging.warning("Host GUI bridge TLS connection failed", exc_info=True)
            self.shutdown_request(request)
        finally:
            self.slots.release()


class HostBridgeAccess:
    def __init__(self, server, home: Path):
        self.server = None
        self.thread = None
        try:
            config = load_bridge_config(home)
            if config is None:
                return
            self.server = BridgeHostServer(server, home, config)
        except (OSError, ValueError, KeyError, TypeError):
            logging.warning("Host GUI bridge is unavailable; direct access is unchanged", exc_info=True)
            return
        self.thread = threading.Thread(
            target=self.server.serve_forever,
            name="xedoc-host-gui-bridge",
            daemon=True,
        )
        try:
            self.thread.start()
        except BaseException:
            self.server.server_close()
            raise

    def close(self):
        if self.server is not None:
            if self.thread is not None and self.thread.is_alive():
                self.server.shutdown()
                self.thread.join(timeout=5)
            self.server.server_close()
