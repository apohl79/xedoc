#!/usr/bin/env python3
"""One-shot Arctic Embed XS inference owned by the reference router extension."""

import json
import math
import os
import secrets
import socket
import socketserver
import subprocess
import sys
import threading
import time
import unicodedata
from pathlib import Path


class ArcticTokenizer:
    """Minimal portable implementation of Arctic XS's BERT WordPiece tokenizer."""

    def __init__(self, tokenizer_path: Path) -> None:
        tokenizer = json.loads(tokenizer_path.read_text(encoding="utf-8"))
        model = tokenizer.get("model")
        if not isinstance(model, dict) or model.get("type") != "WordPiece":
            raise ValueError("Arctic Embed XS requires a WordPiece tokenizer")
        vocab = model.get("vocab")
        if not isinstance(vocab, dict) or not all(
            isinstance(token, str) and isinstance(identifier, int)
            for token, identifier in vocab.items()
        ):
            raise ValueError("Arctic Embed XS tokenizer has an invalid vocabulary")
        self.vocab = vocab
        self.unknown_id = self._required_token("[UNK]")
        self.cls_id = self._required_token("[CLS]")
        self.sep_id = self._required_token("[SEP]")
        self.max_input_chars_per_word = model.get("max_input_chars_per_word")
        if not isinstance(self.max_input_chars_per_word, int):
            raise ValueError("Arctic Embed XS tokenizer has no word length limit")

    def _required_token(self, token: str) -> int:
        identifier = self.vocab.get(token)
        if not isinstance(identifier, int):
            raise ValueError(f"Arctic Embed XS tokenizer is missing {token}")
        return identifier

    def encode_batch(self, texts: list[str]) -> tuple[list[list[int]], list[list[int]]]:
        input_ids: list[list[int]] = []
        attention_masks: list[list[int]] = []
        for text in texts:
            identifiers = [self.cls_id, *self.tokenize(text)[:510], self.sep_id]
            mask = [1] * len(identifiers)
            identifiers.extend([0] * (512 - len(identifiers)))
            mask.extend([0] * (512 - len(mask)))
            input_ids.append(identifiers)
            attention_masks.append(mask)
        return input_ids, attention_masks

    def tokenize(self, text: str) -> list[int]:
        normalized = self.normalize(text)
        token_ids: list[int] = []
        for token in self.split_punctuation(normalized):
            token_ids.extend(self.wordpiece(token))
        return token_ids

    @staticmethod
    def normalize(text: str) -> str:
        cleaned = []
        for character in text:
            codepoint = ord(character)
            if (
                codepoint == 0
                or codepoint == 0xFFFD
                or ArcticTokenizer.is_control(character)
            ):
                continue
            if ArcticTokenizer.is_whitespace(character):
                cleaned.append(" ")
            elif ArcticTokenizer.is_chinese(character):
                cleaned.extend((" ", character, " "))
            else:
                cleaned.append(character)
        return "".join(
            character
            for character in unicodedata.normalize("NFD", "".join(cleaned).lower())
            if unicodedata.category(character) != "Mn"
        )

    @staticmethod
    def is_whitespace(character: str) -> bool:
        return character.isspace() or ord(character) in (0x0009, 0x000A, 0x000D)

    @staticmethod
    def is_control(character: str) -> bool:
        return not ArcticTokenizer.is_whitespace(character) and unicodedata.category(
            character
        ).startswith("C")

    @staticmethod
    def is_chinese(character: str) -> bool:
        codepoint = ord(character)
        return (
            0x4E00 <= codepoint <= 0x9FFF
            or 0x3400 <= codepoint <= 0x4DBF
            or 0x20000 <= codepoint <= 0x2A6DF
            or 0x2A700 <= codepoint <= 0x2B73F
            or 0x2B740 <= codepoint <= 0x2B81F
            or 0x2B820 <= codepoint <= 0x2CEAF
            or 0xF900 <= codepoint <= 0xFAFF
            or 0x2F800 <= codepoint <= 0x2FA1F
        )

    @staticmethod
    def split_punctuation(text: str) -> list[str]:
        tokens: list[str] = []
        current: list[str] = []
        for character in text:
            if ArcticTokenizer.is_whitespace(character):
                if current:
                    tokens.append("".join(current))
                    current = []
            elif unicodedata.category(character).startswith("P"):
                if current:
                    tokens.append("".join(current))
                    current = []
                tokens.append(character)
            else:
                current.append(character)
        if current:
            tokens.append("".join(current))
        return tokens

    def wordpiece(self, token: str) -> list[int]:
        if len(token) > self.max_input_chars_per_word:
            return [self.unknown_id]
        identifiers: list[int] = []
        start = 0
        while start < len(token):
            end = len(token)
            identifier = None
            while start < end:
                piece = token[start:end]
                if start:
                    piece = f"##{piece}"
                identifier = self.vocab.get(piece)
                if identifier is not None:
                    break
                end -= 1
            if identifier is None:
                return [self.unknown_id]
            identifiers.append(identifier)
            start = end
        return identifiers


def load_embedder(runtime_dir: Path):
    site_packages = runtime_dir / "site-packages"
    sys.path.insert(0, str(site_packages))

    import numpy
    import onnxruntime

    tokenizer = ArcticTokenizer(runtime_dir / "arctic-embed-xs" / "tokenizer.json")
    session = onnxruntime.InferenceSession(
        str(runtime_dir / "arctic-embed-xs" / "onnx" / "model.onnx"),
        providers=["CPUExecutionProvider"],
    )

    def embed(texts):
        input_ids, attention_masks = tokenizer.encode_batch(texts)
        inputs = {
            "input_ids": numpy.array(input_ids, dtype=numpy.int64),
            "attention_mask": numpy.array(attention_masks, dtype=numpy.int64),
            "token_type_ids": numpy.zeros((len(texts), 512), dtype=numpy.int64),
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
    shutdown_token = secrets.token_hex(16)

    class Handler(socketserver.StreamRequestHandler):
        def handle(self):
            try:
                request = json.loads(self.rfile.readline(1024 * 1024))
                if (
                    request.get("command") == "shutdown"
                    and request.get("token") == shutdown_token
                ):
                    response = {"stopped": True}
                    threading.Thread(
                        target=self.server.shutdown, daemon=True
                    ).start()
                else:
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
            json.dumps(
                {
                    "pid": os.getpid(),
                    "port": server.server_address[1],
                    "token": shutdown_token,
                }
            ),
            encoding="utf-8",
        )
        os.replace(temporary, state_path)
        server.serve_forever()
    return 0


def start_daemon(runtime_dir: Path, state_path: Path) -> int:
    if state_path.is_file():
        try:
            state = json.loads(state_path.read_text(encoding="utf-8"))
            port = state["port"]
            if isinstance(port, int):
                with socket.create_connection(("127.0.0.1", port), timeout=1):
                    return 0
        except (OSError, ValueError, KeyError, json.JSONDecodeError):
            pass
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


def stop_daemon(state_path: Path) -> int:
    try:
        state = json.loads(state_path.read_text(encoding="utf-8"))
        port = state["port"]
        token = state["token"]
        if not isinstance(port, int) or not isinstance(token, str):
            raise ValueError("invalid semantic embedder state")
    except (OSError, ValueError, KeyError, json.JSONDecodeError):
        return 1

    try:
        with socket.create_connection(("127.0.0.1", port), timeout=2) as connection:
            connection.sendall(
                (json.dumps({"command": "shutdown", "token": token}) + "\n").encode()
            )
            response = json.loads(connection.makefile(encoding="utf-8").readline())
            if response.get("stopped") is not True:
                raise RuntimeError("semantic embedder daemon rejected shutdown")
    except (OSError, ValueError, KeyError, json.JSONDecodeError) as error:
        raise RuntimeError(f"failed to stop semantic embedder daemon: {error}") from error
    state_path.unlink(missing_ok=True)
    return 0


def main() -> int:
    if len(sys.argv) not in (2, 4):
        raise ValueError("expected an installed model-router runtime directory")
    runtime_dir = Path(sys.argv[1])
    if len(sys.argv) == 4 and sys.argv[2] == "--daemon":
        return daemon(runtime_dir, Path(sys.argv[3]))
    if len(sys.argv) == 4 and sys.argv[2] == "--start-daemon":
        return start_daemon(runtime_dir, Path(sys.argv[3]))
    if len(sys.argv) == 4 and sys.argv[2] == "--stop-daemon":
        return stop_daemon(Path(sys.argv[3]))
    request = json.load(sys.stdin)
    json.dump(
        {"embeddings": load_embedder(runtime_dir)(request_texts(request))},
        sys.stdout,
        separators=(",", ":"),
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
