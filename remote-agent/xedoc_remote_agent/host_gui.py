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
import socket
import ssl
import threading
from typing import Callable
from urllib.parse import parse_qs, urlsplit

from .errors import BrokerError, ErrorCode
from .host_auth import HostAuth
from .host_gui_config import HostGuiConfig
from .host_workspace import HostWorkspaces
from .host_session_gui import HostSessionGuis, proxy_session_gui
from .work_admission import WorkAdmissionGate


class HostServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(
        self,
        address,
        origin: str,
        home: Path,
        *,
        bootstrap_path: Path | None = None,
        on_workspace_config_changed: Callable[[], None] | None = None,
        work_admission: WorkAdmissionGate | None = None,
    ):
        self.address_family = socket.AF_INET6 if ":" in address[0] else socket.AF_INET
        self.origin = origin
        self.authority = urlsplit(origin).netloc
        self.secure = origin.startswith("https://")
        self.auth = HostAuth(home)
        if not self.auth.users:
            raise ValueError("Run host-gui setup before serving")
        self.workspaces = HostWorkspaces(
            home,
            bootstrap_path=bootstrap_path,
            on_config_changed=on_workspace_config_changed,
            work_admission=work_admission,
        )
        self.session_guis = HostSessionGuis(home, self.workspaces._bootstrap_path)
        self.slots = threading.BoundedSemaphore(48)
        self.tls_context = None
        super().__init__(address, HostHandler)

    def origin_for_authority(self, authority: str) -> str:
        scheme = "https" if self.secure else "http"
        return f"{scheme}://{authority}"

    def server_close(self):
        self.session_guis.close()
        super().server_close()

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


class HostGuiService:
    def __init__(
        self,
        home: Path,
        bootstrap_path: Path,
        *,
        on_workspace_config_changed: Callable[[], None] | None = None,
        work_admission: WorkAdmissionGate | None = None,
    ) -> None:
        self.home = home
        self.bootstrap_path = bootstrap_path
        self.on_workspace_config_changed = on_workspace_config_changed
        self.work_admission = work_admission
        self.config = HostGuiConfig.load(home)
        self.origin = self.config.origin
        self.server: HostServer | None = None
        self.thread: threading.Thread | None = None

    def start(self) -> bool:
        if not HostAuth(self.home).users:
            return False
        tls_context = self.config.create_tls_context()
        server = HostServer(
            (self.config.listen_address, self.config.port),
            self.config.origin,
            self.home,
            bootstrap_path=self.bootstrap_path,
            on_workspace_config_changed=self.on_workspace_config_changed,
            work_admission=self.work_admission,
        )
        server.tls_context = tls_context
        thread = threading.Thread(
            target=server.serve_forever,
            name="xedoc-host-gui",
            daemon=True,
        )
        try:
            thread.start()
        except BaseException:
            server.server_close()
            server.workspaces.close()
            raise
        self.server = server
        self.thread = thread
        return True

    def stop(self) -> None:
        server, self.server = self.server, None
        thread, self.thread = self.thread, None
        if server is None:
            return
        server.shutdown()
        server.server_close()
        server.workspaces.close()
        if thread is not None:
            thread.join(timeout=5)

    def has_active_turns(self) -> bool:
        server = self.server
        return server is not None and server.workspaces.has_active_turns()


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

    def reply(
        self,
        status,
        value,
        *,
        content_type="application/json",
        cookie=None,
        session_frame=False,
    ):
        body = value if isinstance(value, bytes) else json.dumps(value).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header(
            "Content-Security-Policy",
            (
                "default-src 'none'; base-uri 'none'; connect-src 'self'; "
                "img-src 'self' data:; object-src blob: data:; form-action 'none'; "
                "frame-ancestors 'self'; script-src 'self'; style-src 'self'; "
                "style-src-attr 'unsafe-inline'"
                if session_frame
                else "default-src 'self'; script-src 'self'; style-src 'self'; "
                "object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'"
            ),
        )
        if cookie is not None:
            self.send_header(
                "Set-Cookie",
                f"xedoc_host={cookie}; Path=/; HttpOnly; SameSite=Strict; "
                f"Max-Age={604800 if cookie else 0}"
                + ("; Secure" if self.server.secure else ""),
            )
        self.end_headers()
        self.wfile.write(body)

    def dispatch(self):
        try:
            authority = self.headers.get("Host", "")
            if not authority or (
                not self.server.secure and authority != self.server.authority
            ):
                self.reply(403, {"error": "Unexpected host"})
                return
            parsed = urlsplit(self.path)
            path = parsed.path
            body = {}
            if self.command == "POST":
                if (
                    self.headers.get("Origin")
                    != self.server.origin_for_authority(authority)
                ):
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
            if self.command == "GET" and path in (
                "/",
                "/host.js",
                "/activity.js",
                "/host.css",
            ):
                asset = {
                    "/": "host.html",
                    "/host.js": "host.js",
                    "/activity.js": "activity.js",
                    "/host.css": "host.css",
                }[path]
                kind = {
                    "/": "text/html; charset=utf-8",
                    "/host.js": "text/javascript",
                    "/activity.js": "text/javascript",
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
            if self.command == "GET" and path.startswith("/session-ui/"):
                parts = path.split("/", 3)
                if len(parts) != 4:
                    self.reply(404, {"error": "Not found"})
                    return
                gui_token, resource = parts[2:]
                entry = self.server.session_guis.entry(gui_token, account)
                proxy_session_gui(self, entry, gui_token, resource, token)
                return
            if self.command == "POST" and path == "/api/logout":
                self.server.auth.logout(token)
                self.server.session_guis.close(account["username"])
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
                hostname = socket.gethostname().split(".", 1)[0]
                self.reply(200, {**state, "account": account, "hostname": hostname})
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
                "/api/session/gui",
            ):
                if not admin and workspace_id not in account["workspaces"]:
                    self.reply(403, {"error": "Workspace access denied"})
                    return
                project_name = (
                    body.get("projectName")
                    if self.command == "POST"
                    else parse_qs(parsed.query).get("projectName", [""])[0]
                )
                if self.command == "POST" and path == "/api/session/gui":
                    thread_id = body.get("threadId")
                    with service._lock:
                        project = service._project_path(workspace_id, project_name)
                        service._thread_for_project(thread_id, project)
                        result = service._request(
                            "thread/webUi/open", {"threadId": thread_id}
                        )
                    self.reply(
                        200,
                        self.server.session_guis.open(
                            account["username"],
                            workspace_id,
                            project_name,
                            thread_id,
                            project,
                            result["url"],
                        ),
                    )
                    return
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
                            archived=body.get("archived") is True,
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
                    self.server.session_guis.close(
                        account["username"], body.get("threadId")
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
                    self.reply(
                        200,
                        {
                            "projects": service.projects(
                                workspace_id,
                                activity_only=parse_qs(parsed.query).get(
                                    "activity", [""]
                                )[0]
                                == "1",
                            )
                        },
                    )
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
                self.server.session_guis.close(workspace_id=workspace_id)
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
        if auth.users:
            print("Host GUI administrator is already configured.")
            reconfigure = input("Configure its listener settings? [y/N]: ").strip()
            if reconfigure.casefold() not in ("y", "yes"):
                return 0
        else:
            username = input("Administrator username: ").strip()
            password = getpass.getpass("Password (at least 6 characters): ")
            if password != getpass.getpass("Confirm password: "):
                parser.error("Passwords do not match")
            try:
                auth.save_user(username, "admin", [], password)
            except ValueError as error:
                parser.error(str(error))

        remote_access = input(
            "Enable remote access over HTTPS on your private network? [y/N]: "
        ).strip()
        try:
            if remote_access.casefold() in ("y", "yes"):
                origin = input(
                    "HTTPS origin clients will visit "
                    "(for example, https://xedoc.local:50000): "
                ).strip()
                origin_parts = urlsplit(origin)
                if (
                    origin_parts.scheme != "https"
                    or not origin_parts.hostname
                    or origin_parts.path
                    or origin_parts.query
                    or origin_parts.fragment
                    or origin_parts.username
                    or origin_parts.password
                ):
                    raise ValueError(
                        "Remote Host GUI origins must be an exact HTTPS origin."
                    )
                hostname = origin_parts.hostname.lower()
                authority = f"[{hostname}]" if ":" in hostname else hostname
                port = origin_parts.port or 443
                origin = f"https://{authority}"
                if port != 443:
                    origin = f"{origin}:{port}"
                default_listener = "::" if ":" in hostname else "0.0.0.0"
                listen_address = (
                    input(
                        "Listener address "
                        f"[{default_listener}; use :: for IPv6-only networks]: "
                    ).strip()
                    or default_listener
                )
                use_existing_certificate = input(
                    "Use an existing TLS certificate/key instead of generating a "
                    "private CA and server certificate? [y/N]: "
                ).strip()
                if use_existing_certificate.casefold() in ("y", "yes"):
                    cert = Path(input("TLS certificate chain PEM path: ").strip())
                    key = Path(input("TLS private key PEM path: ").strip())
                    tls_cert = cert.expanduser().resolve()
                    tls_key = key.expanduser().resolve()
                    ca_cert = None
                else:
                    tls_cert, tls_key, ca_cert = HostGuiConfig.generate_tls_material(
                        args.xedoc_home, hostname
                    )
                config = HostGuiConfig(
                    listen_address,
                    port,
                    origin,
                    tls_cert,
                    tls_key,
                )
                config.create_tls_context()
            else:
                config = HostGuiConfig.local_only()
            config.save(args.xedoc_home)
        except (OSError, ValueError, ssl.SSLError) as error:
            parser.error(str(error))

        print(f"Host GUI configured for {config.origin}.")
        if remote_access.casefold() in ("y", "yes") and ca_cert:
            print(
                "HTTPS certificate generated. To avoid browser security warnings, "
                "install this public CA certificate as trusted on each client:"
            )
            print(f"  {ca_cert}")
            print(
                "Trusting this private CA lets it issue certificates for any "
                "hostname; keep host-gui/ca-key.pem private. Existing CA "
                "material is reused when reconfiguring the listener."
            )
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
