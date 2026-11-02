"""Private local panel launch requests and capability-scoped HTTP transport."""

import http.client
import json
import os
import queue
import socket
import socketserver
import stat
import threading
from urllib.parse import urlsplit

from .errors import BrokerError


class PanelBroker:
    def __init__(self, panels):
        self.server = None
        self.thread = None
        self.path = panels.home / "remote-agent" / "host-panels.sock"
        if not hasattr(socket, "AF_UNIX") or not hasattr(socketserver, "UnixStreamServer"):
            return
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if self.path.exists():
            if not stat.S_ISSOCK(self.path.lstat().st_mode):
                raise ValueError("Plugin panel socket path is not a socket")
            with socket.socket(socket.AF_UNIX) as probe:
                try:
                    probe.connect(str(self.path))
                except ConnectionRefusedError:
                    self.path.unlink()
                else:
                    raise ValueError("A host GUI panel broker is already running")

        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                self.connection.settimeout(240)
                try:
                    line = self.rfile.readline(65537)
                    if len(line) > 65536 or not line.endswith(b"\n"):
                        raise ValueError("Expected a bounded panel request")
                    request = json.loads(line)
                    if not isinstance(request, dict):
                        raise ValueError("Expected a panel request")
                    panel = panels.open_local(request)
                    result = {"ok": True, "title": panel["title"]}
                except BrokerError as error:
                    result = {"ok": False, "error": error.message,
                              "unavailable": error.code.value == "unavailable"
                              and error.message == "No active host GUI session"}
                except (OSError, ValueError, TypeError):
                    result = {"ok": False, "error": "Invalid panel launch request"}
                self.wfile.write((json.dumps(result) + "\n").encode())

        class Server(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
            daemon_threads = True
            request_queue_size = 8

        self.server = Server(str(self.path), Handler)
        os.chmod(self.path, 0o600)
        self.thread = threading.Thread(
            target=self.server.serve_forever, daemon=True, name="host-panel-broker"
        )
        self.thread.start()

    def close(self):
        if self.server is not None:
            self.server.shutdown()
            self.server.server_close()
            self.thread.join(timeout=3)
            self.path.unlink(missing_ok=True)


def proxy_plugin_panel(handler, panels, path, query):
    parts = path.split("/", 3)
    if len(parts) != 4:
        raise BrokerError.not_found()
    token, resource = parts[2:]
    entry = panels.entry(token)
    prefix = f"/plugin-ui/{token}/"
    origin = handler.server.origin_for_authority(handler.headers.get("Host", ""))
    request_origin = handler.headers.get("Origin")
    if (
        request_origin not in (None, "null", origin)
        or handler.command not in {"GET", "HEAD", "OPTIONS"} and request_origin is None
        or resource.startswith("/") or "\\" in resource
        or any(segment in (".", "..") for segment in urlsplit(resource).path.split("/"))
    ):
        raise BrokerError.unauthorized()

    def headers(status, content_type):
        handler.send_response(status)
        handler.send_header("Content-Type", content_type)
        handler.send_header("Cache-Control", "no-store")
        handler.send_header("X-Content-Type-Options", "nosniff")
        handler.send_header("Referrer-Policy", "no-referrer")
        handler.send_header("Access-Control-Allow-Origin", "null")
        handler.send_header("Vary", "Origin")
        handler.send_header("Content-Security-Policy", (
            f"default-src 'none'; script-src {origin}{prefix}; "
            f"style-src {origin}{prefix} 'unsafe-inline'; "
            f"connect-src {origin}{prefix}; img-src {origin}{prefix} data: blob:; "
            f"font-src {origin}{prefix} data:; base-uri {origin}{prefix}; "
            f"frame-ancestors {origin}; form-action 'none'; "
            "sandbox allow-scripts allow-forms allow-downloads"
        ))

    if handler.command == "OPTIONS":
        headers(204, "text/plain")
        handler.send_header("Access-Control-Allow-Methods", "GET, HEAD, POST, PUT, PATCH, DELETE, OPTIONS")
        handler.send_header("Access-Control-Allow-Headers", "Content-Type, Last-Event-ID")
        handler.send_header("Content-Length", "0")
        handler.end_headers()
        return
    size = int(handler.headers.get("Content-Length", "0"))
    limit = 16 * 1024 * 1024
    if not 0 <= size <= limit or handler.headers.get("Transfer-Encoding"):
        raise BrokerError.limit_exceeded()
    body = handler.rfile.read(size) if size else None
    address = entry["address"]
    connection = http.client.HTTPConnection(address.hostname, address.port, timeout=30)
    upstream_socket = None
    reader = None
    try:
        connection.request(
            handler.command, "/" + resource + ("?" + query if query else ""),
            body=body,
            headers={
                "Origin": f"http://{address.netloc}",
                "Content-Type": handler.headers.get("Content-Type", "application/json"),
                "Last-Event-ID": handler.headers.get("Last-Event-ID", ""),
            },
        )
        upstream_socket = connection.sock
        response = connection.getresponse()
        if 300 <= response.status < 400:
            raise BrokerError.unavailable("Plugin redirects are not supported")
        content_type = response.getheader("Content-Type", "application/octet-stream")
        if content_type.startswith("text/html"):
            content = response.read(limit + 1)
            if len(content) > limit:
                raise BrokerError.limit_exceeded()
            content = content.replace(b'<base href="/">', f'<base href="{prefix}">'.encode())
            headers(response.status, content_type)
            handler.send_header("Content-Length", str(len(content)))
            handler.end_headers()
            if handler.command != "HEAD":
                handler.wfile.write(content)
            return
        length = response.getheader("Content-Length")
        if length is not None and not 0 <= int(length) <= limit:
            raise BrokerError.limit_exceeded()
        headers(response.status, content_type)
        if length is not None:
            handler.send_header("Content-Length", length)
        handler.end_headers()
        if handler.command == "HEAD":
            return
        chunks = queue.Queue(maxsize=2)
        stopped = threading.Event()
        upstream_socket.settimeout(None)

        def read():
            try:
                while not stopped.is_set():
                    chunk = response.read1(65536)
                    while not stopped.is_set():
                        try:
                            chunks.put(chunk, timeout=1)
                            break
                        except queue.Full:
                            continue
                    if not chunk:
                        return
            except (OSError, http.client.HTTPException):
                try:
                    chunks.put(b"", timeout=1)
                except queue.Full:
                    pass

        reader = threading.Thread(target=read, daemon=True, name="plugin-panel-proxy")
        reader.start()
        total = 0
        try:
            while True:
                try:
                    panels.entry(token)
                except (BrokerError, PermissionError):
                    break
                try:
                    chunk = chunks.get(timeout=1)
                except queue.Empty:
                    continue
                if not chunk:
                    break
                total += len(chunk)
                if total > limit and not content_type.startswith("text/event-stream"):
                    break
                try:
                    handler.wfile.write(chunk)
                    handler.wfile.flush()
                except OSError:
                    break
        finally:
            stopped.set()
    finally:
        if upstream_socket is not None:
            try:
                upstream_socket.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
        if reader is not None:
            reader.join(timeout=3)
        if reader is None or not reader.is_alive():
            connection.close()
        handler.close_connection = True
