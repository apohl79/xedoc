"""Authenticated host workspace inventory, separate from session relays."""

import argparse
import getpass
from http.cookies import SimpleCookie
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from importlib.resources import files
import json
import logging
import os
from pathlib import Path
import ssl
import threading
from urllib.parse import parse_qs, urlsplit

from .errors import BrokerError, ErrorCode
from .host_auth import HostAuth
from .host_workspace import HostWorkspaces


class HostServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address, origin: str, home: Path):
        self.origin = origin
        self.authority = urlsplit(origin).netloc
        self.secure = origin.startswith("https://")
        self.auth = HostAuth(home)
        if not self.auth.users:
            raise ValueError("Run host-gui setup before serving")
        self.workspaces = HostWorkspaces(home)
        self.slots = threading.BoundedSemaphore(16)
        self.tls_context = None
        super().__init__(address, HostHandler)

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
            if self.tls_context is not None:
                request = self.tls_context.wrap_socket(request, server_side=True)
            super().process_request_thread(request, client_address)
        except OSError:
            self.shutdown_request(request)
        finally:
            self.slots.release()


class HostHandler(BaseHTTPRequestHandler):
    server: HostServer

    def setup(self):
        super().setup()
        self.connection.settimeout(15)

    def log_message(self, format, *args):
        pass

    def do_GET(self):
        self.dispatch()

    def do_POST(self):
        self.dispatch()

    def reply(self, status, value, *, content_type="application/json", cookie=None):
        body = value if isinstance(value, bytes) else json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header(
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self'; "
            "object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        )
        if cookie is not None:
            self.send_header(
                "Set-Cookie",
                f"xedoc_host={cookie}; Path=/; HttpOnly; SameSite=Strict; "
                f"Max-Age={43200 if cookie else 0}"
                + ("; Secure" if self.server.secure else ""),
            )
        self.end_headers()
        self.wfile.write(body)

    def dispatch(self):
        try:
            if self.headers.get("Host") != self.server.authority:
                self.reply(403, {"error": "Unexpected host"})
                return
            parsed = urlsplit(self.path)
            path = parsed.path
            body = {}
            if self.command == "POST":
                if self.headers.get("Origin") != self.server.origin:
                    self.reply(403, {"error": "Unexpected origin"})
                    return
                size = int(self.headers.get("Content-Length", "0"))
                if (
                    not 0 < size <= 64 * 1024
                    or self.headers.get_content_type() != "application/json"
                ):
                    raise ValueError("Expected bounded JSON request")
                body = json.loads(self.rfile.read(size))
                if not isinstance(body, dict):
                    raise ValueError("Expected JSON object")
            if self.command == "GET" and path in ("/", "/host.js", "/host.css"):
                asset = {
                    "/": "host.html",
                    "/host.js": "host.js",
                    "/host.css": "host.css",
                }[path]
                kind = {
                    "/": "text/html; charset=utf-8",
                    "/host.js": "text/javascript",
                    "/host.css": "text/css",
                }[path]
                self.reply(
                    200,
                    files("xedoc_remote_agent")
                    .joinpath("host_assets", asset)
                    .read_bytes(),
                    content_type=kind,
                )
                return
            if self.command == "POST" and path == "/api/login":
                token = self.server.auth.login(
                    body.get("username"), body.get("password"), self.client_address[0]
                )
                self.reply(200, {"ok": True}, cookie=token)
                return
            cookies = SimpleCookie(self.headers.get("Cookie", ""))
            token = cookies["xedoc_host"].value if "xedoc_host" in cookies else ""
            account = self.server.auth.account(token)
            admin = account["role"] == "admin"
            service = self.server.workspaces
            if self.command == "POST" and path == "/api/logout":
                self.server.auth.logout(token)
                self.reply(200, {"ok": True}, cookie="")
                return
            if self.command == "GET" and path == "/api/state":
                state = service.snapshot()
                allowed = (
                    {entry["workspaceId"] for entry in state["workspaces"]}
                    if admin
                    else set(account["workspaces"])
                )
                state["workspaces"] = [
                    entry
                    for entry in state["workspaces"]
                    if entry["workspaceId"] in allowed
                ]
                self.reply(200, {**state, "account": account})
                return
            workspace_id = (
                body.get("workspaceId")
                if self.command == "POST"
                else parse_qs(parsed.query).get("workspaceId", [""])[0]
            )
            if path in (
                "/api/project/sessions",
                "/api/session/open",
                "/api/session/new",
                "/api/session/data",
                "/api/session/close",
                "/api/session/turn",
            ):
                if not admin and workspace_id not in account["workspaces"]:
                    self.reply(403, {"error": "Workspace access denied"})
                    return
                project_name = (
                    body.get("projectName")
                    if self.command == "POST"
                    else parse_qs(parsed.query).get("projectName", [""])[0]
                )
                if self.command == "GET" and path == "/api/project/sessions":
                    include_history = (
                        parse_qs(parsed.query).get("includeHistory", [""])[0] == "1"
                    )
                    sessions = service.project_sessions(
                        account["username"],
                        workspace_id,
                        project_name,
                        include_history=include_history,
                    )
                    self.reply(200, {"sessions": sessions})
                    return
                if self.command == "POST" and path == "/api/session/new":
                    self.reply(
                        200,
                        service.new_session(
                            account["username"], workspace_id, project_name
                        ),
                    )
                    return
                if self.command == "POST" and path == "/api/session/open":
                    self.reply(
                        200,
                        service.resume_session(
                            account["username"],
                            workspace_id,
                            project_name,
                            body.get("threadId"),
                        ),
                    )
                    return
                if self.command == "GET" and path == "/api/session/data":
                    query = parse_qs(parsed.query)
                    thread_id = query.get("threadId", [""])[0]
                    self.reply(
                        200,
                        service.session_data(
                            account["username"],
                            workspace_id,
                            project_name,
                            thread_id,
                            query.get("cursor", [None])[0],
                        ),
                    )
                    return
                if self.command == "POST" and path == "/api/session/close":
                    service.close_session(
                        account["username"],
                        workspace_id,
                        project_name,
                        body.get("threadId"),
                    )
                    self.reply(200, {"ok": True})
                    return
                if self.command == "POST" and path == "/api/session/turn":
                    result = service.start_turn(
                        account["username"],
                        workspace_id,
                        project_name,
                        body.get("threadId"),
                        body.get("message"),
                    )
                    self.reply(200, result)
                    return
                self.reply(404, {"error": "Not found"})
                return
            if path in ("/api/projects", "/api/project/create"):
                if not admin and workspace_id not in account["workspaces"]:
                    self.reply(403, {"error": "Workspace access denied"})
                    return
                if self.command == "GET" and path == "/api/projects":
                    self.reply(200, {"projects": service.projects(workspace_id)})
                    return
                if self.command == "POST" and path == "/api/project/create":
                    service.create_project(workspace_id, body.get("name"))
                    self.reply(200, {"ok": True})
                    return
            if not admin:
                self.reply(403, {"error": "Administrator access required"})
                return
            if self.command == "GET" and path == "/api/users":
                self.reply(200, {"users": self.server.auth.public_users()})
                return
            if self.command == "GET" and path == "/api/browse":
                self.reply(
                    200,
                    service.browse(
                        parse_qs(parsed.query).get("path", [str(Path.home())])[0]
                    ),
                )
                return
            if self.command == "POST" and path == "/api/workspace/add":
                service.add(workspace_id, body.get("root"), body.get("create") is True)
            elif self.command == "POST" and path == "/api/workspace/remove":
                service.remove(workspace_id)
            elif self.command == "POST" and path == "/api/user/save":
                assignments = body.get("workspaces", [])
                known = {
                    entry["workspaceId"] for entry in service.snapshot()["workspaces"]
                }
                if not isinstance(assignments, list) or any(
                    value not in known for value in assignments
                ):
                    raise ValueError("Unknown workspace assignment")
                self.server.auth.save_user(
                    body.get("username"),
                    body.get("role"),
                    assignments,
                    body.get("password", ""),
                )
            elif self.command == "POST" and path == "/api/user/remove":
                self.server.auth.delete_user(body.get("username"))
            else:
                self.reply(404, {"error": "Not found"})
                return
            self.reply(200, {"ok": True})
        except PermissionError as error:
            self.reply(401, {"error": str(error)})
        except (ValueError, TypeError, KeyError) as error:
            self.reply(400, {"error": str(error)})
        except BrokerError as error:
            status = {
                ErrorCode.INVALID_REQUEST: 400,
                ErrorCode.UNAUTHORIZED: 403,
                ErrorCode.NOT_FOUND: 404,
                ErrorCode.CONFLICT: 409,
                ErrorCode.LIMIT_EXCEEDED: 413,
                ErrorCode.UNAVAILABLE: 503,
                ErrorCode.INTERNAL: 500,
            }[error.code]
            self.reply(status, {"error": error.message})
        except OSError:
            self.reply(
                503,
                {
                    "error": "Host service unavailable; check the controller and workspace configuration"
                },
            )
        except Exception:
            logging.exception("Host GUI request failed")
            self.reply(500, {"error": "Host request failed"})


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="xedoc-remote-agentd host-gui")
    parser.add_argument("action", choices=("setup", "serve"))
    parser.add_argument(
        "--xedoc-home",
        type=Path,
        default=Path(os.environ.get("XEDOC_HOME", Path.home() / ".xedoc")),
    )
    parser.add_argument("--listen", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=50000)
    parser.add_argument("--origin")
    parser.add_argument("--tls-cert")
    parser.add_argument("--tls-key")
    parser.add_argument("--allow-insecure-http", action="store_true")
    args = parser.parse_args(argv)
    if args.action == "setup":
        auth = HostAuth(args.xedoc_home)
        username = input("Administrator username: ").strip()
        password = getpass.getpass("Password (at least 12 characters): ")
        if password != getpass.getpass("Confirm password: "):
            parser.error("Passwords do not match")
        auth.save_user(username, "admin", [], password)
        return 0
    origin = urlsplit(args.origin or "")
    if (
        origin.scheme not in ("http", "https")
        or not origin.netloc
        or origin.path
        or origin.query
        or origin.fragment
        or origin.username
    ):
        parser.error(
            "--origin must be the exact browser origin, e.g. https://hostname:50000"
        )
    if origin.scheme == "https" and not (args.tls_cert and args.tls_key):
        parser.error("HTTPS requires --tls-cert and --tls-key")
    if origin.scheme == "http" and not args.allow_insecure_http:
        parser.error(
            "Use TLS, or explicitly opt into plaintext credentials with --allow-insecure-http"
        )
    if origin.scheme == "http" and (args.tls_cert or args.tls_key):
        parser.error("TLS certificates require an HTTPS origin")
    with HostServer((args.listen, args.port), args.origin, args.xedoc_home) as server:
        if origin.scheme == "https":
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(args.tls_cert, args.tls_key)
            server.tls_context = context
        print(f"Host GUI: {args.origin}", flush=True)
        try:
            server.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            server.workspaces.close()
    return 0
