#!/usr/bin/env python3
"""Replay one captured Xedoc Responses request with a prompt variant."""

import argparse
import base64
import copy
import hashlib
import json
import os
import secrets
import socket
import ssl
import sys
import struct
from urllib.parse import urlparse
import urllib.error
import urllib.request
from pathlib import Path


def fail(message: str) -> None:
    raise ValueError(message)


def load_request(path: Path, index: int) -> tuple[str, dict]:
    matches = []
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.strip():
            continue
        event = json.loads(line)
        if event.get("event") == "sampling_request":
            matches.append(event)
    if not matches:
        fail(f"no sampling_request events found in {path}")
    try:
        event = matches[index]
    except IndexError as exc:
        raise ValueError(
            f"request index {index} is out of range; found {len(matches)} requests"
        ) from exc
    transport = event.get("transport")
    if transport not in {"http", "websocket"}:
        fail(f"unsupported transport {transport!r}")
    body = event.get("body")
    if not isinstance(body, dict):
        fail("sampling_request body must be a JSON object")
    return transport, body


def replace_prompt(body: dict, prompt: str) -> dict:
    result = copy.deepcopy(body)
    input_items = result.get("input")
    if not isinstance(input_items, list):
        fail("request body has no input array")
    for item in input_items:
        if (
            isinstance(item, dict)
            and item.get("type") == "message"
            and item.get("role") == "developer"
        ):
            content = item.get("content")
            if (
                isinstance(content, list)
                and len(content) == 1
                and isinstance(content[0], dict)
                and content[0].get("type") == "input_text"
                and isinstance(content[0].get("text"), str)
            ):
                content[0]["text"] = prompt
                return result
    fail("request has no replaceable single-text developer prompt")


def send_request(endpoint: str, api_key: str, body: dict) -> bytes:
    request = urllib.request.Request(
        endpoint,
        data=json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8"),
        headers={
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        },
        method="POST",
    )
    try:
        with urllib.request.urlopen(request) as response:
            return response.read()
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"backend returned HTTP {exc.code}: {detail}") from exc


def websocket_frame(payload: bytes) -> bytes:
    length = len(payload)
    if length < 126:
        header = bytes((0x81, 0x80 | length))
    elif length < 65536:
        header = bytes((0x81, 0x80 | 126)) + struct.pack("!H", length)
    else:
        header = bytes((0x81, 0x80 | 127)) + struct.pack("!Q", length)
    mask = secrets.token_bytes(4)
    return header + mask + bytes(value ^ mask[index % 4] for index, value in enumerate(payload))


def recv_exact(connection: socket.socket, size: int) -> bytes:
    payload = bytearray()
    while len(payload) < size:
        chunk = connection.recv(size - len(payload))
        if not chunk:
            fail("backend closed the WebSocket before a complete frame")
        payload.extend(chunk)
    return bytes(payload)


def read_websocket_frame(connection: socket.socket) -> tuple[int, bytes]:
    header = recv_exact(connection, 2)
    first, second = header
    length = second & 0x7F
    if length == 126:
        length = struct.unpack("!H", recv_exact(connection, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", recv_exact(connection, 8))[0]
    mask = recv_exact(connection, 4) if second & 0x80 else None
    payload = bytearray(recv_exact(connection, length))
    if mask:
        payload = bytearray(
            value ^ mask[index % 4] for index, value in enumerate(payload)
        )
    return first & 0x0F, bytes(payload)


def send_websocket(endpoint: str, api_key: str, body: dict) -> bytes:
    parsed = urlparse(endpoint)
    if parsed.scheme not in {"ws", "wss"} or not parsed.hostname:
        fail("WebSocket endpoint must use ws:// or wss://")
    port = parsed.port or (443 if parsed.scheme == "wss" else 80)
    connection = socket.create_connection((parsed.hostname, port), timeout=60)
    if parsed.scheme == "wss":
        connection = ssl.create_default_context().wrap_socket(
            connection, server_hostname=parsed.hostname
        )
    key = secrets.token_bytes(16)
    key_text = base64.b64encode(key).decode("ascii")
    path = parsed.path or "/"
    if parsed.query:
        path += f"?{parsed.query}"
    handshake = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {parsed.hostname}:{port}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key_text}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        f"Authorization: Bearer {api_key}\r\n\r\n"
    ).encode("ascii")
    connection.sendall(handshake)
    response = bytearray()
    while b"\r\n\r\n" not in response:
        chunk = connection.recv(4096)
        if not chunk:
            fail("backend closed the WebSocket handshake")
        response.extend(chunk)
    if not response.startswith(b"HTTP/1.1 101"):
        fail(f"WebSocket handshake failed: {response.splitlines()[0].decode(errors='replace')}")
    headers = response.split(b"\r\n\r\n", 1)[0].decode("iso-8859-1").splitlines()[1:]
    response_headers = {
        key.strip().lower(): value.strip()
        for line in headers
        if ":" in line
        for key, value in [line.split(":", 1)]
    }
    expected_accept = base64.b64encode(
        hashlib.sha1(key_text.encode("ascii") + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest()
    ).decode("ascii")
    if response_headers.get("sec-websocket-accept") != expected_accept:
        fail("WebSocket handshake has an invalid Sec-WebSocket-Accept header")
    connection.sendall(websocket_frame(
        json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    ))
    messages = []
    while True:
        opcode, payload = read_websocket_frame(connection)
        if opcode == 0x8:
            break
        if opcode == 0x1:
            messages.append(payload)
        if opcode == 0x9:
            connection.sendall(websocket_frame(payload))
    connection.close()
    return b"\n".join(messages)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("trace", type=Path)
    parser.add_argument("--endpoint", required=True)
    parser.add_argument("--api-key-env", default="OPENAI_API_KEY")
    parser.add_argument("--request-index", type=int, default=0)
    parser.add_argument("--prompt-file", type=Path)
    parser.add_argument("--body-output", type=Path)
    parser.add_argument("--send", action="store_true")
    args = parser.parse_args()
    try:
        transport, body = load_request(args.trace, args.request_index)
        if args.prompt_file:
            body = replace_prompt(
                body, args.prompt_file.read_text(encoding="utf-8")
            )
        encoded = json.dumps(body, ensure_ascii=False, indent=2) + "\n"
        if args.body_output:
            args.body_output.write_text(encoded, encoding="utf-8")
        if not args.send:
            sys.stdout.write(encoded)
            return 0
        api_key = os.environ.get(args.api_key_env)
        if not api_key:
            fail(f"environment variable {args.api_key_env} is not set")
        response = (
            send_request(args.endpoint, api_key, body)
            if transport == "http"
            else send_websocket(args.endpoint, api_key, body)
        )
        sys.stdout.buffer.write(response)
        sys.stdout.write("\n")
        return 0
    except (OSError, json.JSONDecodeError, RuntimeError, ValueError) as exc:
        print(f"replay_request: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
