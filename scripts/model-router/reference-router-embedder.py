#!/usr/bin/env python3
"""One-shot Arctic Embed XS inference owned by the reference router extension."""

import json
import math
import os
import socketserver
import subprocess
import sys
import time
from pathlib import Path


def load_embedder(runtime_dir: Path):
    site_packages = runtime_dir / "site-packages"
    sys.path.insert(0, str(site_packages))

    import numpy
    import onnxruntime
    from tokenizers import Tokenizer

    tokenizer = Tokenizer.from_file(
        str(runtime_dir / "arctic-embed-xs" / "tokenizer.json")
    )
    tokenizer.enable_truncation(max_length=512)
    tokenizer.enable_padding(length=512)
    session = onnxruntime.InferenceSession(
        str(runtime_dir / "arctic-embed-xs" / "onnx" / "model.onnx"),
        providers=["CPUExecutionProvider"],
    )

    def embed(texts):
        encodings = tokenizer.encode_batch(texts)
        inputs = {
            "input_ids": numpy.array(
                [encoding.ids for encoding in encodings], dtype=numpy.int64
            ),
            "attention_mask": numpy.array(
                [encoding.attention_mask for encoding in encodings], dtype=numpy.int64
            ),
            "token_type_ids": numpy.array(
                [encoding.type_ids for encoding in encodings], dtype=numpy.int64
            ),
        }
        output = session.run(None, inputs)[0]
        embeddings = []
        for values in output[:, 0, :]:
            magnitude = math.sqrt(float(numpy.dot(values, values)))
            if not math.isfinite(magnitude) or magnitude == 0.0:
                raise ValueError("Arctic Embed XS returned an invalid embedding")
            embeddings.append((values / magnitude).tolist())
        return embeddings

    return embed


def request_texts(request):
    texts = request.get("texts")
    if (
        not isinstance(texts, list)
        or not texts
        or any(not isinstance(text, str) for text in texts)
    ):
        raise ValueError("embedding request must contain non-empty string texts")
    return texts


def daemon(runtime_dir: Path, state_path: Path) -> int:
    embed = load_embedder(runtime_dir)

    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            try:
                request = json.loads(self.rfile.readline(1024 * 1024))
                response = {"embeddings": embed(request_texts(request))}
            except (ValueError, json.JSONDecodeError) as error:
                response = {"error": str(error)[:256]}
            self.wfile.write(
                (json.dumps(response, separators=(",", ":")) + "\n").encode()
            )

    class Server(socketserver.ThreadingTCPServer):
        allow_reuse_address = True
        daemon_threads = True

    with Server(("127.0.0.1", 0), Handler) as server:
        temporary = state_path.with_suffix(".tmp")
        temporary.write_text(
            json.dumps({"pid": os.getpid(), "port": server.server_address[1]}),
            encoding="utf-8",
        )
        os.replace(temporary, state_path)
        server.serve_forever()
    return 0


def start_daemon(runtime_dir: Path, state_path: Path) -> int:
    state_path.unlink(missing_ok=True)
    command = [sys.executable, __file__, str(runtime_dir), "--daemon", str(state_path)]
    kwargs = {
        "stdin": subprocess.DEVNULL,
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.DEVNULL,
    }
    if os.name == "nt":
        kwargs["creationflags"] = subprocess.CREATE_NEW_PROCESS_GROUP
    else:
        kwargs["start_new_session"] = True
    subprocess.Popen(command, **kwargs)
    for _ in range(100):
        if state_path.is_file():
            return 0
        time.sleep(0.1)
    raise RuntimeError("semantic embedder daemon did not start")


def main() -> int:
    if len(sys.argv) not in (2, 4):
        raise ValueError("expected an installed model-router runtime directory")
    runtime_dir = Path(sys.argv[1])
    if len(sys.argv) == 4 and sys.argv[2] == "--daemon":
        return daemon(runtime_dir, Path(sys.argv[3]))
    if len(sys.argv) == 4 and sys.argv[2] == "--start-daemon":
        return start_daemon(runtime_dir, Path(sys.argv[3]))
    request = json.load(sys.stdin)
    json.dump(
        {"embeddings": load_embedder(runtime_dir)(request_texts(request))},
        sys.stdout,
        separators=(",", ":"),
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
