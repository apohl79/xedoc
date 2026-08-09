#!/usr/bin/env python3
"""Stream microphone audio through an app-server realtime thread.

The app-server realtime WebSocket API carries signed 16-bit, little-endian PCM.
The default format is mono at 24 kHz, which is the format selected by the
Realtime Voice (v2) backend. Install the script dependencies with:

    uv run --project scripts scripts/codex_realtime_audio.py THREAD_ID

The realtime conversation feature must be enabled in the Codex configuration,
and the supplied thread must be available to the app-server.
"""

from __future__ import annotations

import argparse
import asyncio
import base64
from contextlib import suppress
from dataclasses import dataclass
import json
import os
from pathlib import Path
import sys
from typing import Protocol, cast

try:
    import sounddevice as sd
except ModuleNotFoundError:  # pragma: no cover - exercised by the CLI guard.
    sd = None  # type: ignore[assignment]

try:
    import websockets
except ModuleNotFoundError:  # pragma: no cover - exercised by the CLI guard.
    websockets = None  # type: ignore[assignment]


DEFAULT_CODEX_HOME = Path.home() / ".codex"
DEFAULT_SAMPLE_RATE = 24_000
DEFAULT_CHANNELS = 1
DEFAULT_BLOCK_SIZE = 480
PCM_SAMPLE_WIDTH = 2

JsonObject = dict[str, object]


class WebSocket(Protocol):
    async def send(self, message: str) -> None: ...

    async def recv(self) -> str | bytes: ...

    async def close(self) -> None: ...


class RealtimeClientError(RuntimeError):
    """Raised when app-server rejects the realtime session or audio stream."""


def default_socket_path() -> Path:
    codex_home = Path(os.environ.get("CODEX_HOME", DEFAULT_CODEX_HOME))
    return codex_home / "app-server-control" / "app-server-control.sock"


def positive_int(value: str) -> int:
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def _decode_json(message: str | bytes) -> JsonObject:
    try:
        value = json.loads(message)
    except json.JSONDecodeError as error:
        raise RealtimeClientError("app-server returned invalid JSON") from error
    if not isinstance(value, dict):
        raise RealtimeClientError("app-server returned a non-object JSON message")
    return cast(JsonObject, value)


def _error_message(error: object) -> str:
    if isinstance(error, dict):
        message = error.get("message")
        if isinstance(message, str):
            return message
    return "app-server request failed"


class JsonRpcClient:
    def __init__(self, websocket: WebSocket) -> None:
        self.websocket = websocket
        self.next_request_id = 1
        self.send_lock = asyncio.Lock()

    async def _send(self, message: JsonObject) -> None:
        async with self.send_lock:
            await self.websocket.send(json.dumps(message, separators=(",", ":")))

    async def notify(self, method: str) -> None:
        await self._send({"method": method})

    async def request(self, method: str, params: JsonObject) -> JsonObject:
        request_id = self.next_request_id
        self.next_request_id += 1
        await self._send({"method": method, "id": request_id, "params": params})

        while True:
            message = _decode_json(await self.websocket.recv())
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise RealtimeClientError(_error_message(message["error"]))
            result = message.get("result", {})
            if not isinstance(result, dict):
                raise RealtimeClientError("app-server returned a non-object result")
            return cast(JsonObject, result)

    async def send_request(self, method: str, params: JsonObject) -> None:
        request_id = self.next_request_id
        self.next_request_id += 1
        await self._send({"method": method, "id": request_id, "params": params})


def build_start_params(
    thread_id: str,
    *,
    model: str | None,
    voice: str | None,
    version: str,
    include_startup_context: bool,
    prompt: str | None,
) -> JsonObject:
    params: JsonObject = {
        "threadId": thread_id,
        "outputModality": "audio",
        "version": version,
    }
    if model is not None:
        params["model"] = model
    if voice is not None:
        params["voice"] = voice
    if not include_startup_context:
        params["includeStartupContext"] = False
    if prompt is not None:
        params["prompt"] = prompt
    return params


def build_audio_params(
    thread_id: str,
    audio: bytes,
    *,
    sample_rate: int,
    channels: int,
) -> JsonObject:
    samples_per_channel = len(audio) // PCM_SAMPLE_WIDTH // channels
    return {
        "threadId": thread_id,
        "audio": {
            "data": base64.b64encode(audio).decode("ascii"),
            "sampleRate": sample_rate,
            "numChannels": channels,
            "samplesPerChannel": samples_per_channel,
        },
    }


def _require_dependencies() -> None:
    missing = [
        name
        for name, module in (("websockets", websockets), ("sounddevice", sd))
        if module is None
    ]
    if missing:
        names = " ".join(missing)
        raise RealtimeClientError(
            f"missing Python package(s): {names}; run "
            "`uv run --project scripts scripts/codex_realtime_audio.py ...`"
        )


async def _connect(socket_path: Path, url: str | None) -> WebSocket:
    _require_dependencies()
    assert websockets is not None
    try:
        if url is not None:
            return cast(
                WebSocket,
                await websockets.connect(
                    url, max_size=None, proxy=None, compression=None
                ),
            )
        if not socket_path.exists():
            raise RealtimeClientError(
                f"app-server socket does not exist: {socket_path}"
            )
        return cast(
            WebSocket,
            await websockets.unix_connect(
                str(socket_path),
                uri="ws://localhost/rpc",
                max_size=None,
                proxy=None,
                compression=None,
            ),
        )
    except RealtimeClientError:
        raise
    except OSError as error:
        endpoint = url or str(socket_path)
        raise RealtimeClientError(
            f"could not connect to app-server at {endpoint}: {error}"
        ) from error


@dataclass
class AudioPlayer:
    device: str | None
    stream: object | None = None
    sample_rate: int | None = None
    channels: int | None = None

    def _write(self, audio: bytes, sample_rate: int, channels: int) -> None:
        assert sd is not None
        if (
            self.stream is None
            or self.sample_rate != sample_rate
            or self.channels != channels
        ):
            self.close()
            self.stream = sd.RawOutputStream(
                samplerate=sample_rate,
                channels=channels,
                dtype="int16",
                device=self.device,
            )
            self.stream.start()
            self.sample_rate = sample_rate
            self.channels = channels
        self.stream.write(audio)  # type: ignore[attr-defined]

    async def write(self, audio: bytes, sample_rate: int, channels: int) -> None:
        if audio:
            await asyncio.to_thread(self._write, audio, sample_rate, channels)

    def close(self) -> None:
        stream = self.stream
        if stream is not None:
            with suppress(Exception):
                stream.stop()  # type: ignore[attr-defined]
            with suppress(Exception):
                stream.close()  # type: ignore[attr-defined]
        self.stream = None
        self.sample_rate = None
        self.channels = None


class TranscriptPrinter:
    def __init__(self) -> None:
        self.role: str | None = None

    def delta(self, role: str, text: str) -> None:
        if self.role != role:
            if self.role is not None:
                sys.stdout.write("\n")
            label = "You" if role == "user" else "Assistant"
            sys.stdout.write(f"{label}: ")
            self.role = role
        sys.stdout.write(text)
        sys.stdout.flush()

    def done(self) -> None:
        if self.role is not None:
            sys.stdout.write("\n")
            sys.stdout.flush()
            self.role = None


async def _receive_events(
    websocket: WebSocket,
    stop_event: asyncio.Event,
    player: AudioPlayer,
    transcript: TranscriptPrinter,
) -> None:
    while not stop_event.is_set():
        message = _decode_json(await websocket.recv())
        if "id" in message:
            if "error" in message:
                raise RealtimeClientError(_error_message(message["error"]))
            continue
        method = message.get("method")
        params = message.get("params")
        if not isinstance(method, str) or not isinstance(params, dict):
            continue
        if method == "thread/realtime/transcript/delta":
            role = params.get("role")
            delta = params.get("delta")
            if isinstance(role, str) and isinstance(delta, str):
                transcript.delta(role, delta)
        elif method == "thread/realtime/transcript/done":
            transcript.done()
        elif method == "thread/realtime/outputAudio/delta":
            audio = params.get("audio")
            if not isinstance(audio, dict):
                continue
            encoded = audio.get("data")
            sample_rate = audio.get("sampleRate")
            channels = audio.get("numChannels")
            if (
                not isinstance(encoded, str)
                or not isinstance(sample_rate, int)
                or not isinstance(channels, int)
            ):
                continue
            try:
                decoded = base64.b64decode(encoded, validate=True)
            except ValueError as error:
                raise RealtimeClientError(
                    "app-server returned invalid output audio"
                ) from error
            await player.write(decoded, sample_rate, channels)
        elif method == "thread/realtime/error":
            message_text = params.get("message", "unknown realtime error")
            raise RealtimeClientError(str(message_text))
        elif method == "thread/realtime/closed":
            reason = params.get("reason")
            if reason:
                print(f"\nRealtime session closed: {reason}", flush=True)
            stop_event.set()


async def _send_microphone(
    rpc: JsonRpcClient,
    thread_id: str,
    stop_event: asyncio.Event,
    audio_queue: asyncio.Queue[tuple[bytes, bool]],
    *,
    sample_rate: int,
    channels: int,
) -> None:
    while not stop_event.is_set():
        try:
            data, overflowed = await asyncio.wait_for(audio_queue.get(), 0.1)
        except TimeoutError:
            continue
        if overflowed:
            print("\nWarning: microphone input overflow", file=sys.stderr, flush=True)
        await rpc.send_request(
            "thread/realtime/appendAudio",
            build_audio_params(
                thread_id,
                bytes(data),
                sample_rate=sample_rate,
                channels=channels,
            ),
        )


async def run(args: argparse.Namespace) -> None:
    _require_dependencies()
    assert sd is not None
    websocket = await _connect(args.socket, args.url)
    rpc = JsonRpcClient(websocket)
    player = AudioPlayer(args.output_device)
    stop_event = asyncio.Event()
    receive_task: asyncio.Task[None] | None = None
    send_task: asyncio.Task[None] | None = None
    try:
        await rpc.request(
            "initialize",
            {
                "clientInfo": {
                    "name": "codex-realtime-audio",
                    "title": "Codex realtime audio client",
                    "version": "0.1.0",
                },
                "capabilities": {"experimentalApi": True},
            },
        )
        await rpc.notify("initialized")
        await rpc.request(
            "thread/realtime/start",
            build_start_params(
                args.thread_id,
                model=args.model,
                voice=args.voice,
                version=args.version,
                include_startup_context=args.include_startup_context,
                prompt=args.prompt,
            ),
        )
        print(f"Realtime audio connected to thread {args.thread_id}.", flush=True)
        print("Press Ctrl+C to stop.", flush=True)

        loop = asyncio.get_running_loop()
        audio_queue: asyncio.Queue[tuple[bytes, bool]] = asyncio.Queue(maxsize=8)

        def on_audio(
            data: object,
            _frames: int,
            _time: object,
            status: object,
        ) -> None:
            chunk = (bytes(data), bool(getattr(status, "input_overflow", False)))

            def enqueue() -> None:
                if stop_event.is_set():
                    return
                if audio_queue.full():
                    with suppress(asyncio.QueueEmpty):
                        audio_queue.get_nowait()
                with suppress(asyncio.QueueFull):
                    audio_queue.put_nowait(chunk)

            with suppress(RuntimeError):
                loop.call_soon_threadsafe(enqueue)

        with sd.RawInputStream(
            samplerate=args.sample_rate,
            blocksize=args.block_size,
            channels=args.channels,
            dtype="int16",
            device=args.input_device,
            callback=on_audio,
        ) as _input_stream:
            receive_task = asyncio.create_task(
                _receive_events(websocket, stop_event, player, TranscriptPrinter())
            )
            send_task = asyncio.create_task(
                _send_microphone(
                    rpc,
                    args.thread_id,
                    stop_event,
                    audio_queue,
                    sample_rate=args.sample_rate,
                    channels=args.channels,
                )
            )
            done, _ = await asyncio.wait(
                (receive_task, send_task), return_when=asyncio.FIRST_COMPLETED
            )
            for task in done:
                error = task.exception()
                if error is not None:
                    raise error
    finally:
        stop_event.set()
        if send_task is not None:
            send_task.cancel()
            with suppress(asyncio.CancelledError):
                await send_task
        with suppress(Exception):
            await rpc.send_request("thread/realtime/stop", {"threadId": args.thread_id})
        if receive_task is not None:
            receive_task.cancel()
            with suppress(asyncio.CancelledError):
                await receive_task
        player.close()
        with suppress(Exception):
            await websocket.close()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Capture microphone PCM and stream it through an app-server realtime thread."
    )
    parser.add_argument("thread_id", help="Existing app-server thread ID")
    endpoint = parser.add_mutually_exclusive_group()
    endpoint.add_argument(
        "--socket",
        type=Path,
        default=default_socket_path(),
        help="App-server Unix socket (default: $CODEX_HOME/app-server-control/app-server-control.sock)",
    )
    endpoint.add_argument(
        "--url",
        help="App-server WebSocket URL, for example ws://127.0.0.1:4500/rpc",
    )
    parser.add_argument("--model", help="Realtime model override")
    parser.add_argument("--voice", help="Realtime voice override")
    parser.add_argument("--version", choices=("v1", "v2", "v3"), default="v2")
    parser.add_argument("--prompt", help="Optional realtime backend prompt override")
    parser.add_argument(
        "--no-startup-context",
        action="store_false",
        dest="include_startup_context",
        help="Do not include Codex startup context in the realtime session",
    )
    parser.add_argument(
        "--sample-rate",
        type=positive_int,
        default=DEFAULT_SAMPLE_RATE,
        help=f"Microphone sample rate (default: {DEFAULT_SAMPLE_RATE})",
    )
    parser.add_argument(
        "--channels",
        type=positive_int,
        default=DEFAULT_CHANNELS,
        help=f"Microphone channel count (default: {DEFAULT_CHANNELS})",
    )
    parser.add_argument(
        "--block-size",
        type=positive_int,
        default=DEFAULT_BLOCK_SIZE,
        help=f"Microphone frames per request (default: {DEFAULT_BLOCK_SIZE})",
    )
    parser.add_argument("--input-device", help="sounddevice input device name or ID")
    parser.add_argument("--output-device", help="sounddevice output device name or ID")
    parser.add_argument(
        "--list-devices",
        action="store_true",
        help="List sounddevice devices and exit",
    )
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if args.list_devices:
            _require_dependencies()
            assert sd is not None
            print(sd.query_devices())
            return 0
        asyncio.run(run(args))
    except (RealtimeClientError, OSError, RuntimeError) as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        print("\nRealtime audio stopped.", file=sys.stderr)
        return 130
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
