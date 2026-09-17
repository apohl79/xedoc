"""Dependency-free client SDK for app-server session scripts.

The SDK supports both script transports: a normal app-server WebSocket for a
controller and the host-provided JSONL stdin/stdout connection for a configured
session child. It deliberately exposes the common JSON-RPC request primitive as
well as the session-script registration helpers, so a script does not need a
separate client implementation for either surface.
"""

import base64
from collections.abc import Callable
import hashlib
import json
import secrets
import socket
import struct
import sys
from typing import Any, Optional
from urllib.parse import urlparse


MAX_MESSAGE_BYTES = 1 << 20
MAX_BUFFERED_RESPONSES = 64


class RpcError(RuntimeError):
    """The peer sent invalid JSON-RPC or closed its transport unexpectedly."""


class WebSocketTransport:
    """A bounded, dependency-free JSON WebSocket transport for app-server RPC."""

    def __init__(self, endpoint: str, timeout: float) -> None:
        parsed = urlparse(endpoint)
        if parsed.scheme != "ws" or not parsed.hostname or not parsed.port:
            raise RpcError("endpoint must be a ws:// URL with an explicit port")
        self.socket = socket.create_connection((parsed.hostname, parsed.port), timeout)
        self.socket.settimeout(timeout)
        self.buffer = b""
        path = parsed.path or "/"
        if parsed.query:
            path = f"{path}?{parsed.query}"
        key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
        self.socket.sendall(
            (
                f"GET {path} HTTP/1.1\r\nHost: {parsed.hostname}:{parsed.port}\r\n"
                "Upgrade: websocket\r\nConnection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            ).encode("ascii")
        )
        response = self._read_until(b"\r\n\r\n")
        expected = base64.b64encode(
            hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")
            ).digest()
        ).decode("ascii")
        if (
            not response.startswith(b"HTTP/1.1 101")
            or expected.encode() not in response
        ):
            raise RpcError("app-server rejected the websocket handshake")

    def close(self) -> None:
        try:
            self._send(0x8, b"")
        except OSError:
            pass
        self.socket.close()

    def send_json(self, value: dict[str, Any]) -> None:
        self._send(0x1, json.dumps(value, separators=(",", ":")).encode())

    def receive_json(self) -> dict[str, Any]:
        fragments: list[bytes] = []
        opcode: Optional[int] = None
        while True:
            fin, frame_opcode, payload = self._receive()
            if frame_opcode == 0x8:
                raise RpcError("app-server closed the connection")
            if frame_opcode == 0x9:
                self._send(0xA, payload)
                continue
            if frame_opcode == 0xA:
                continue
            if frame_opcode == 0x1 and opcode is None:
                opcode = frame_opcode
            elif frame_opcode != 0x0 or opcode is None:
                raise RpcError("invalid websocket message sequence")
            fragments.append(payload)
            if sum(map(len, fragments)) > MAX_MESSAGE_BYTES:
                raise RpcError("websocket message exceeds the size limit")
            if fin:
                return _decode_json_object(b"".join(fragments))

    def _send(self, opcode: int, payload: bytes) -> None:
        if len(payload) > MAX_MESSAGE_BYTES:
            raise RpcError("websocket message exceeds the size limit")
        mask = secrets.token_bytes(4)
        payload = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        length = len(payload)
        if length < 126:
            header = bytes((0x80 | opcode, 0x80 | length))
        elif length <= 0xFFFF:
            header = bytes((0x80 | opcode, 0x80 | 126)) + struct.pack("!H", length)
        else:
            header = bytes((0x80 | opcode, 0x80 | 127)) + struct.pack("!Q", length)
        self.socket.sendall(header + mask + payload)

    def _receive(self) -> tuple[bool, int, bytes]:
        first, second = self._read_exact(2)
        payload_length = second & 0x7F
        if payload_length == 126:
            payload_length = struct.unpack("!H", self._read_exact(2))[0]
        elif payload_length == 127:
            payload_length = struct.unpack("!Q", self._read_exact(8))[0]
        if payload_length > MAX_MESSAGE_BYTES:
            raise RpcError("websocket message exceeds the size limit")
        mask = self._read_exact(4) if second & 0x80 else b""
        payload = self._read_exact(payload_length)
        if mask:
            payload = bytes(
                value ^ mask[index % 4] for index, value in enumerate(payload)
            )
        return bool(first & 0x80), first & 0x0F, payload

    def _read_exact(self, size: int) -> bytes:
        while len(self.buffer) < size:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise RpcError("app-server closed the connection")
            self.buffer += chunk
        result, self.buffer = self.buffer[:size], self.buffer[size:]
        return result

    def _read_until(self, marker: bytes) -> bytes:
        while marker not in self.buffer:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise RpcError("app-server closed the connection")
            self.buffer += chunk
            if len(self.buffer) > 64 * 1024:
                raise RpcError("websocket handshake exceeds the size limit")
        end = self.buffer.index(marker) + len(marker)
        result, self.buffer = self.buffer[:end], self.buffer[end:]
        return result


class HostChildTransport:
    """Bounded JSONL framing for the host-managed stdin/stdout child connection."""

    def close(self) -> None:
        return

    def send_json(self, value: dict[str, Any]) -> None:
        encoded = json.dumps(value, separators=(",", ":"))
        if len(encoded.encode()) > MAX_MESSAGE_BYTES:
            raise RpcError("JSON-RPC message exceeds the size limit")
        print(encoded, flush=True)

    def receive_json(self) -> dict[str, Any]:
        line = sys.stdin.buffer.readline(MAX_MESSAGE_BYTES + 1)
        if not line:
            raise RpcError("app-server closed the child pipe")
        if len(line) > MAX_MESSAGE_BYTES:
            raise RpcError("JSON-RPC message exceeds the size limit")
        return _decode_json_object(line)


def _decode_json_object(payload: bytes) -> dict[str, Any]:
    try:
        value = json.loads(payload)
    except json.JSONDecodeError as error:
        raise RpcError("app-server returned invalid JSON") from error
    if not isinstance(value, dict):
        raise RpcError("app-server returned a non-object JSON message")
    return value


class SessionScriptClient:
    """A client for session-script RPC over a controller or child transport."""

    def __init__(self, transport: Any) -> None:
        self._transport = transport
        self._next_id = 1
        self._notification_handler: Optional[Callable[[dict[str, Any]], None]] = None
        self._server_request_handler: Optional[
            Callable[[dict[str, Any]], dict[str, Any]]
        ] = None
        self._buffered_responses: dict[int | str, dict[str, Any]] = {}

    @classmethod
    def connect_websocket(cls, endpoint: str, timeout: float) -> "SessionScriptClient":
        return cls(WebSocketTransport(endpoint, timeout))

    @classmethod
    def from_host_child(cls) -> "SessionScriptClient":
        return cls(HostChildTransport())

    def set_notification_handler(
        self, notification_handler: Callable[[dict[str, Any]], None]
    ) -> None:
        self._notification_handler = notification_handler

    def set_server_request_handler(
        self, server_request_handler: Callable[[dict[str, Any]], dict[str, Any]]
    ) -> None:
        self._server_request_handler = server_request_handler

    def initialize(self, client_name: str, title: str, version: str) -> None:
        self.request(
            "initialize",
            {
                "clientInfo": {
                    "name": client_name,
                    "title": title,
                    "version": version,
                },
                "capabilities": {"experimentalApi": True},
            },
        )
        self._transport.send_json({"method": "initialized"})

    def register(
        self,
        thread_id: str,
        script_id: str,
        name: str,
        version: str,
        subscriptions: dict[str, Any],
        requested_capabilities: list[str],
    ) -> dict[str, Any]:
        return self.request(
            "script/register",
            {
                "threadId": thread_id,
                "script": {"id": script_id, "name": name, "version": version},
                "subscriptions": subscriptions,
                "requestedCapabilities": requested_capabilities,
            },
        )

    def read(self, registration_id: str) -> dict[str, Any]:
        return self.request("script/read", {"registrationId": registration_id})

    def respond(
        self,
        registration_id: str,
        prompt_id: str,
        response_lease: str,
        response: dict[str, Any],
    ) -> dict[str, Any]:
        return self.request(
            "script/respond",
            {
                "registrationId": registration_id,
                "promptId": prompt_id,
                "responseLease": response_lease,
                "response": response,
            },
        )

    def unregister(self, registration_id: str) -> dict[str, Any]:
        return self.request("script/unregister", {"registrationId": registration_id})

    def request(self, method: str, params: dict[str, Any]) -> dict[str, Any]:
        request_id = self._next_id
        self._next_id += 1
        self._transport.send_json(
            {"method": method, "id": request_id, "params": params}
        )
        message = self._wait_for_response(request_id)
        error = message.get("error")
        if isinstance(error, dict):
            raise RpcError(str(error.get("message", "app-server request failed")))
        result = message.get("result", {})
        if not isinstance(result, dict):
            raise RpcError("app-server returned a non-object result")
        return result

    def request_error(self, method: str, params: dict[str, Any]) -> str:
        request_id = self._next_id
        self._next_id += 1
        self._transport.send_json(
            {"method": method, "id": request_id, "params": params}
        )
        message = self._wait_for_response(request_id)
        error = message.get("error")
        if not isinstance(error, dict) or not isinstance(error.get("message"), str):
            raise RpcError(f"{method} unexpectedly succeeded")
        return error["message"]

    def _wait_for_response(self, request_id: int) -> dict[str, Any]:
        while True:
            buffered = self._buffered_responses.pop(request_id, None)
            if buffered is not None:
                return buffered
            message = self.receive_message()
            if message.get("method") is not None:
                self._handle_server_message(message)
                continue
            response_id = message.get("id")
            if response_id == request_id:
                return message
            if not isinstance(response_id, (int, str)) or isinstance(response_id, bool):
                raise RpcError("app-server returned a response with an invalid id")
            if len(self._buffered_responses) >= MAX_BUFFERED_RESPONSES:
                raise RpcError("app-server exceeded the buffered response limit")
            self._buffered_responses[response_id] = message

    def receive_message(self) -> dict[str, Any]:
        return self._transport.receive_json()

    def handle_message(self, message: dict[str, Any]) -> None:
        if message.get("method") is not None:
            self._handle_server_message(message)

    def send_result(self, request_id: Any, result: dict[str, Any]) -> None:
        self._transport.send_json({"id": request_id, "result": result})

    def send_error(self, request_id: Any, code: int, message: str) -> None:
        self._transport.send_json(
            {"id": request_id, "error": {"code": code, "message": message}}
        )

    def close(self) -> None:
        self._transport.close()

    def _handle_notification(self, message: dict[str, Any]) -> None:
        if self._notification_handler is not None:
            self._notification_handler(message)

    def _handle_server_message(self, message: dict[str, Any]) -> None:
        request_id = message.get("id")
        if request_id is None:
            self._handle_notification(message)
        elif self._server_request_handler is None:
            self.send_error(
                request_id, -32601, "script does not handle server requests"
            )
        else:
            self.send_result(request_id, self._server_request_handler(message))
