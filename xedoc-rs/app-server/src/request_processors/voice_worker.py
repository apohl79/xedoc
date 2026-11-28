"""Local Whisper transcription and Piper synthesis worker."""

import base64
import hashlib
import io
import json
import os
import shutil
import sys
import tarfile
import tempfile
import time
import urllib.request
import wave
from contextlib import contextmanager
from pathlib import Path, PurePosixPath

import numpy
import sherpa_onnx


WHISPER = {
    "name": "whisper-tiny",
    "url": (
        "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/"
        "sherpa-onnx-whisper-tiny.tar.bz2"
    ),
    "sha256": "c46116994e539aa165266d96b325252728429c12535eb9d8b6a2b10f129e66b1",
    "size": 116204861,
    "directory": "sherpa-onnx-whisper-tiny",
    "files": (
        "tiny-encoder.int8.onnx",
        "tiny-decoder.int8.onnx",
        "tiny-tokens.txt",
    ),
}
TTS_MODELS = {
    "en": {
        "name": "vits-piper-en_US-joe-medium-int8",
        "url": (
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/"
            "vits-piper-en_US-joe-medium-int8.tar.bz2"
        ),
        "sha256": "644527f29eca0ada5595d7b6e4daf6388f5ac0c4870b4d42996a6687fcefeb37",
        "size": 21230019,
        "directory": "vits-piper-en_US-joe-medium-int8",
        "model": "en_US-joe-medium.onnx",
        "files": (
            "en_US-joe-medium.onnx",
            "tokens.txt",
            "espeak-ng-data/phondata",
        ),
    },
    "de": {
        "name": "vits-piper-de_DE-thorsten-medium-int8",
        "url": (
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/tts-models/"
            "vits-piper-de_DE-thorsten-medium-int8.tar.bz2"
        ),
        "sha256": "07e240b7b9c1fc9211d5a69512f8cbe11b3286c2ed79c15c076ac6ed427fdf13",
        "size": 20949833,
        "directory": "vits-piper-de_DE-thorsten-medium-int8",
        "model": "de_DE-thorsten-medium.onnx",
        "files": (
            "de_DE-thorsten-medium.onnx",
            "tokens.txt",
            "espeak-ng-data/phondata",
        ),
    },
}
MAX_INPUT_AUDIO_BYTES = 1_000_000
MAX_INPUT_AUDIO_SECONDS = 30
MAX_TTS_TEXT_BYTES = 1200
MAX_TTS_AUDIO_SECONDS = 60
MAX_TTS_AUDIO_BYTES = 2_800_000
MAX_REQUEST_BYTES = 1_500_000
MAX_RESPONSE_BYTES = 4_000_000


@contextmanager
def model_cache_lock(cache: Path, name: str):
    cache.mkdir(parents=True, exist_ok=True)
    with (cache / f".{name}.lock").open("a+b") as lock_file:
        if os.name == "nt":
            import msvcrt

            lock_file.seek(0, os.SEEK_END)
            if lock_file.tell() == 0:
                lock_file.write(b"\0")
                lock_file.flush()
            while True:
                try:
                    lock_file.seek(0)
                    msvcrt.locking(lock_file.fileno(), msvcrt.LK_NBLCK, 1)
                    break
                except OSError:
                    time.sleep(0.1)
        else:
            import fcntl

            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            if os.name == "nt":
                lock_file.seek(0)
                msvcrt.locking(lock_file.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                fcntl.flock(lock_file.fileno(), fcntl.LOCK_UN)


def cached_model_directory(destination: Path, manifest: dict) -> Path | None:
    marker = destination / ".verified"
    if (
        not destination.is_symlink()
        and marker.is_file()
        and not marker.is_symlink()
        and marker.read_text(encoding="ascii") == manifest["sha256"]
        and all((destination / name).is_file() for name in manifest["files"])
    ):
        return destination
    return None


def model_directory(cache: Path, manifest: dict) -> Path:
    destination = cache / manifest["name"]
    with model_cache_lock(cache, manifest["name"]):
        cached = cached_model_directory(destination, manifest)
        if cached:
            return cached

        if destination.is_symlink():
            destination.unlink()
        elif destination.exists():
            shutil.rmtree(destination)

        with tempfile.TemporaryDirectory(
            prefix=f".{manifest['name']}-", dir=cache
        ) as temporary:
            staging = Path(temporary)
            archive_path = staging / "model.tar.bz2"
            digest = hashlib.sha256()
            downloaded = 0
            request = urllib.request.Request(
                manifest["url"],
                headers={"User-Agent": "Xedoc local voice model"},
            )
            with (
                urllib.request.urlopen(request, timeout=60) as response,
                archive_path.open("wb") as archive_file,
            ):
                content_length = response.headers.get("Content-Length")
                if content_length and int(content_length) != manifest["size"]:
                    raise ValueError("The speech model download has an unexpected size")
                while chunk := response.read(1024 * 1024):
                    downloaded += len(chunk)
                    if downloaded > manifest["size"]:
                        raise ValueError("The speech model download is too large")
                    digest.update(chunk)
                    archive_file.write(chunk)
            if downloaded != manifest["size"] or digest.hexdigest() != manifest["sha256"]:
                raise ValueError("The speech model download failed its integrity check")

            extracted = staging / "extracted"
            extracted.mkdir()
            with tarfile.open(archive_path, "r:bz2") as archive:
                members = archive.getmembers()
                for member in members:
                    name = PurePosixPath(member.name)
                    if (
                        name.is_absolute()
                        or ".." in name.parts
                        or not (member.isfile() or member.isdir())
                    ):
                        raise ValueError(
                            "The speech model archive contains an unsafe path"
                        )
                archive.extractall(extracted, members=members, filter="data")

            model = extracted / manifest["directory"]
            if not model.is_dir() or any(
                not (model / name).is_file() for name in manifest["files"]
            ):
                raise ValueError("The speech model archive is incomplete")
            (model / ".verified").write_text(manifest["sha256"], encoding="ascii")
            os.replace(model, destination)
            if not cached_model_directory(destination, manifest):
                raise ValueError("The speech model cache could not be installed")
    return destination


def read_input_audio(encoded: str):
    raw = base64.b64decode(encoded, validate=True)
    if len(raw) > MAX_INPUT_AUDIO_BYTES:
        raise ValueError("The voice recording is too large")
    with wave.open(io.BytesIO(raw), "rb") as audio:
        if (
            audio.getnchannels() != 1
            or audio.getsampwidth() != 2
            or audio.getframerate() != 16000
            or audio.getcomptype() != "NONE"
            or audio.getnframes() > MAX_INPUT_AUDIO_SECONDS * 16000
        ):
            raise ValueError("Voice audio must be mono 16 kHz PCM and under 30 seconds")
        samples = numpy.frombuffer(audio.readframes(audio.getnframes()), dtype="<i2")
    if samples.size == 0:
        raise ValueError("The voice recording is empty")
    return samples.astype(numpy.float32) / 32768.0


def transcribe(request: dict, cache: Path, recognizers: dict):
    language = "de" if str(request.get("locale", "")).lower().startswith("de") else "en"
    manifest = model_directory(cache, WHISPER)
    recognizer = recognizers.get(language)
    if recognizer is None:
        recognizer = sherpa_onnx.OfflineRecognizer.from_whisper(
            encoder=str(manifest / "tiny-encoder.int8.onnx"),
            decoder=str(manifest / "tiny-decoder.int8.onnx"),
            tokens=str(manifest / "tiny-tokens.txt"),
            language=language,
            task="transcribe",
            num_threads=2,
            debug=False,
            provider="cpu",
        )
        recognizers[language] = recognizer
    stream = recognizer.create_stream()
    stream.accept_waveform(16000, read_input_audio(request["audioBase64"]))
    recognizer.decode_stream(stream)
    text = stream.result.text.strip()
    if not text:
        raise ValueError("No speech was recognized")
    return {"text": text[:16000]}


def synthesize(request: dict, cache: Path, synthesizers: dict):
    language = "de" if str(request.get("locale", "")).lower().startswith("de") else "en"
    text = request.get("text")
    if not isinstance(text, str) or not text.strip() or len(text.encode("utf-8")) > MAX_TTS_TEXT_BYTES:
        raise ValueError("Speech text must contain 1–1200 UTF-8 bytes")

    manifest = TTS_MODELS[language]
    model_directory_path = model_directory(cache, manifest)
    synthesizer = synthesizers.get(language)
    if synthesizer is None:
        config = sherpa_onnx.OfflineTtsConfig(
            model=sherpa_onnx.OfflineTtsModelConfig(
                vits=sherpa_onnx.OfflineTtsVitsModelConfig(
                    model=str(model_directory_path / manifest["model"]),
                    tokens=str(model_directory_path / "tokens.txt"),
                    data_dir=str(model_directory_path / "espeak-ng-data"),
                ),
                num_threads=2,
                debug=False,
                provider="cpu",
            ),
            max_num_sentences=2,
        )
        if not config.validate():
            raise ValueError("The local speech model is not valid")
        synthesizer = sherpa_onnx.OfflineTts(config)
        synthesizers[language] = synthesizer

    audio = synthesizer.generate(text, sherpa_onnx.GenerationConfig())
    if (
        len(audio.samples) == 0
        or len(audio.samples) / audio.sample_rate > MAX_TTS_AUDIO_SECONDS
    ):
        raise ValueError("The generated voice response is too long")
    samples = numpy.clip(
        numpy.asarray(audio.samples) * 32767, -32768, 32767
    ).astype("<i2")
    if samples.nbytes > MAX_TTS_AUDIO_BYTES:
        raise ValueError("The generated voice response is too large")
    output = io.BytesIO()
    with wave.open(output, "wb") as wav:
        wav.setnchannels(1)
        wav.setsampwidth(2)
        wav.setframerate(audio.sample_rate)
        wav.writeframes(samples.tobytes())
    return {"audioBase64": base64.b64encode(output.getvalue()).decode("ascii")}


def handle(request: dict, cache: Path, recognizers: dict, synthesizers: dict):
    if not isinstance(request, dict):
        raise ValueError("Voice request must be an object")
    if request.get("action") == "status":
        return {}
    if request.get("action") == "transcribe":
        return transcribe(request, cache, recognizers)
    if request.get("action") == "synthesize":
        return synthesize(request, cache, synthesizers)
    raise ValueError("Unsupported voice action")


def main():
    home = Path(os.environ["XEDOC_HOME"])
    cache = home / "voice" / "models"
    recognizers = {}
    synthesizers = {}
    for line in sys.stdin:
        try:
            if len(line.encode("utf-8")) > MAX_REQUEST_BYTES:
                raise ValueError("Voice request is too large")
            request = json.loads(line)
            response = {"ok": True, **handle(request, cache, recognizers, synthesizers)}
        except Exception as error:
            response = {"ok": False, "error": str(error)[:512]}
        encoded = json.dumps(response, ensure_ascii=False, separators=(",", ":"))
        if len(encoded.encode("utf-8")) > MAX_RESPONSE_BYTES:
            encoded = json.dumps(
                {"ok": False, "error": "Local voice response exceeds the size limit"}
            )
        sys.stdout.write(encoded + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
