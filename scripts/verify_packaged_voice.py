"""Exercise a native package's speech runtime through app-server JSON-RPC."""

import argparse
import base64
from datetime import datetime, timezone
import io
import json
import os
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import wave


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("package_dir", type=Path)
    args = parser.parse_args()
    binary = (
        args.package_dir.resolve()
        / "bin"
        / ("xedoc.exe" if os.name == "nt" else "xedoc")
    )
    with tempfile.TemporaryDirectory(prefix="xedoc-voice-acceptance-") as temporary:
        home = Path(temporary)
        (home / "config.toml").write_text(
            'cli_auth_credentials_store = "file"\n', encoding="utf-8"
        )

        def claim(value):
            return (
                base64.urlsafe_b64encode(json.dumps(value).encode())
                .decode()
                .rstrip("=")
            )

        token = ".".join(
            (
                claim({"alg": "none"}),
                claim(
                    {
                        "email": "voice-acceptance@example.invalid",
                        "https://api.openai.com/auth": {
                            "chatgpt_account_id": "voice-acceptance",
                            "chatgpt_plan_type": "plus",
                        },
                    }
                ),
                "acceptance",
            )
        )
        (home / "auth.json").write_text(
            json.dumps(
                {
                    "auth_mode": "chatgpt",
                    "tokens": {
                        "id_token": token,
                        "access_token": token,
                        "refresh_token": "acceptance-only",
                        "account_id": "voice-acceptance",
                    },
                    "last_refresh": datetime.now(timezone.utc).isoformat(),
                }
            ),
            encoding="utf-8",
        )
        environment = {
            key: value
            for key, value in os.environ.items()
            if key not in ("OPENAI_API_KEY", "XEDOC_API_KEY")
        }
        environment["XEDOC_HOME"] = str(home)
        messages = queue.Queue()
        with (
            (home / "stderr.log").open("w", encoding="utf-8") as stderr,
            subprocess.Popen(
                [str(binary), "app-server", "--stdio"],
                cwd=home,
                env=environment,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=stderr,
                text=True,
                encoding="utf-8",
            ) as server,
        ):

            def receive():
                for line in server.stdout:
                    messages.put(json.loads(line))
                messages.put(None)

            threading.Thread(target=receive, daemon=True).start()
            request_id = 0

            def request(method, params):
                nonlocal request_id
                request_id += 1
                server.stdin.write(
                    json.dumps(
                        {
                            "id": request_id,
                            "method": method,
                            "params": params,
                        }
                    )
                    + "\n"
                )
                server.stdin.flush()
                while True:
                    message = messages.get(timeout=300)
                    if message is None:
                        raise RuntimeError("App-server exited before responding")
                    if message.get("id") == request_id:
                        if "error" in message:
                            raise RuntimeError(message["error"])
                        return message["result"]

            try:
                request(
                    "initialize",
                    {
                        "clientInfo": {"name": "voice_acceptance", "version": "0.1.0"},
                        "capabilities": {"experimentalApi": True},
                    },
                )
                server.stdin.write('{"method":"initialized"}\n')
                server.stdin.flush()
                account = request("account/read", {"refreshToken": False})
                assert account["account"]["type"] == "chatgpt", account
                thread = request("thread/start", {"cwd": str(home), "ephemeral": True})
                thread_id = thread["thread"]["id"]
                params = {"threadId": thread_id}
                status = request("thread/voice/status", params)
                assert status == {"available": True, "unavailableReason": None}, status
                print("PASS: stored OAuth enables the packaged worker", flush=True)

                for locale, text, expected in (
                    ("en", "The quick brown fox jumps over the lazy dog.", "fox"),
                    ("de", "Guten Morgen. Heute ist ein schöner Tag.", "morgen"),
                ):
                    result = request(
                        "thread/voice/synthesize",
                        {
                            **params,
                            "locale": locale,
                            "text": text,
                        },
                    )
                    with wave.open(
                        io.BytesIO(base64.b64decode(result["audioBase64"]))
                    ) as audio:
                        assert audio.getnchannels() == 1 and audio.getsampwidth() == 2
                        assert audio.getnframes() > 0
                        source_rate = audio.getframerate()
                        samples = memoryview(audio.readframes(audio.getnframes())).cast(
                            "h"
                        )
                    resampled = b"".join(
                        samples[
                            min(int(index * source_rate / 16000), len(samples) - 1)
                        ].to_bytes(2, "little", signed=True)
                        for index in range(int(len(samples) * 16000 / source_rate))
                    )
                    output = io.BytesIO()
                    with wave.open(output, "wb") as audio:
                        audio.setnchannels(1)
                        audio.setsampwidth(2)
                        audio.setframerate(16000)
                        audio.writeframes(resampled)
                    result = request(
                        "thread/voice/transcribe",
                        {
                            **params,
                            "locale": locale,
                            "audioBase64": base64.b64encode(output.getvalue()).decode(),
                        },
                    )
                    assert expected in result["text"].lower(), result
                    print(
                        f"PASS: {locale} synthesis → transcription: {result['text']}",
                        flush=True,
                    )
                request("account/logout", {})
                status = request("thread/voice/status", params)
                assert not status["available"], status
                assert "OpenAI" in status["unavailableReason"], status
                print("PASS: missing auth is reported after logout", flush=True)
                request(
                    "account/login/start",
                    {
                        "type": "apiKey",
                        "apiKey": "sk-voice-acceptance-not-a-real-key",
                    },
                )
                status = request("thread/voice/status", params)
                assert status == {"available": True, "unavailableReason": None}, status
                print("PASS: configured API-key auth also enables voice", flush=True)
            finally:
                server.stdin.close()
                try:
                    server.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    server.terminate()
                    server.wait(timeout=10)


if __name__ == "__main__":
    main()
