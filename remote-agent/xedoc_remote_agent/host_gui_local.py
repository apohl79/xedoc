"""Same-machine access, separate from the certificate-protected TLS listener."""

from http.server import ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import tempfile
import threading


class LocalHostServer(ThreadingHTTPServer):
    daemon_threads = True
    # On Windows, SO_REUSEADDR lets other sockets take over the bound port.
    allow_reuse_address = os.name != "nt"
    secure = False
    tls_context = None
    device_pairing = None

    def __init__(self, remote_server):
        self.pairing_server = remote_server
        self.local_access_token = secrets.token_urlsafe(48)
        self.slots = threading.BoundedSemaphore(48)
        super().__init__(("127.0.0.1", 0), remote_server.RequestHandlerClass)
        self.authority = f"127.0.0.1:{self.server_port}"
        self.origin = f"http://{self.authority}"

    def __getattr__(self, name):
        return getattr(self.pairing_server, name)

    def origin_for_authority(self, authority):
        return f"http://{authority}"

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
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()


class LocalHostAccess:
    def __init__(self, server, home: Path):
        self.server = LocalHostServer(server)
        self.thread = None
        self.path = home / "host-gui" / "local.json"
        self.content = json.dumps({
            "origin": self.server.origin,
            "token": self.server.local_access_token,
        })
        try:
            self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            descriptor, temporary = tempfile.mkstemp(dir=self.path.parent)
            try:
                with os.fdopen(descriptor, "w") as stream:
                    stream.write(self.content)
                os.replace(temporary, self.path)
            finally:
                if os.path.exists(temporary):
                    os.unlink(temporary)
            self.thread = threading.Thread(
                target=self.server.serve_forever,
                name="xedoc-host-gui-local",
                daemon=True,
            )
            self.thread.start()
        except BaseException:
            self.close()
            raise

    def close(self):
        if self.thread is not None and self.thread.is_alive():
            self.server.shutdown()
            self.thread.join(timeout=5)
        self.server.server_close()
        try:
            if self.path.read_text() == self.content:
                self.path.unlink()
        except FileNotFoundError:
            pass
