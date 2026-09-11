"""Pinned native ONNX Runtime installation for model-router packages."""

import hashlib
import json
import shutil
import tarfile
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path

from .targets import TargetSpec


@dataclass(frozen=True)
class RuntimeDistribution:
    url: str
    archive_sha256: str
    archive_member: str
    license_member: str
    library_file: str


RUNTIME_DISTRIBUTIONS: dict[str, RuntimeDistribution] = {
    "aarch64-apple-darwin": RuntimeDistribution(
        url="https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-osx-arm64-1.28.0.tgz",
        archive_sha256="1268b359718099bde2cedb55787f182a130067bc4f31e8c88478c445b850d3d8",
        archive_member="onnxruntime-osx-arm64-1.28.0/lib/libonnxruntime.1.28.0.dylib",
        license_member="onnxruntime-osx-arm64-1.28.0/LICENSE",
        library_file="libonnxruntime.1.28.0.dylib",
    ),
    "aarch64-unknown-linux-gnu": RuntimeDistribution(
        url="https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-linux-aarch64-1.28.0.tgz",
        archive_sha256="e15ff8b5d85afe6c144d97c6fd432254bf76a219daaf17658087d6ecb3e8f0bb",
        archive_member="onnxruntime-linux-aarch64-1.28.0/lib/libonnxruntime.so",
        license_member="onnxruntime-linux-aarch64-1.28.0/LICENSE",
        library_file="libonnxruntime.so",
    ),
    "aarch64-pc-windows-msvc": RuntimeDistribution(
        url="https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-win-arm64-1.28.0.zip",
        archive_sha256="cbe4547463ece092b505c3581376ed5896d22b5429f39d5e645e425ecdd369ad",
        archive_member="onnxruntime-win-arm64-1.28.0/lib/onnxruntime.dll",
        license_member="onnxruntime-win-arm64-1.28.0/LICENSE",
        library_file="onnxruntime.dll",
    ),
    "x86_64-unknown-linux-gnu": RuntimeDistribution(
        url="https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-linux-x64-1.28.0.tgz",
        archive_sha256="a3e1b79d7bb1bf09696ce675f49e4064e6c81f6202b8225624fff0e93f8d6407",
        archive_member="onnxruntime-linux-x64-1.28.0/lib/libonnxruntime.so",
        license_member="onnxruntime-linux-x64-1.28.0/LICENSE",
        library_file="libonnxruntime.so",
    ),
    "x86_64-pc-windows-msvc": RuntimeDistribution(
        url="https://github.com/microsoft/onnxruntime/releases/download/v1.28.0/onnxruntime-win-x64-1.28.0.zip",
        archive_sha256="abef733dacbe2f571547a7150b479b5cb9cc0df22f96c24983a42cadb1b4f8bc",
        archive_member="onnxruntime-win-x64-1.28.0/lib/onnxruntime.dll",
        license_member="onnxruntime-win-x64-1.28.0/LICENSE",
        library_file="onnxruntime.dll",
    ),
}


def install_runtime(spec: TargetSpec, destination: Path) -> Path:
    try:
        distribution = RUNTIME_DISTRIBUTIONS[spec.target]
    except KeyError as error:
        raise RuntimeError(
            "Model routing requires a pinned ONNX Runtime for "
            f"package target {spec.target}; no compatible runtime is available."
        ) from error
    archive_path = destination / (
        "download.zip" if distribution.url.endswith(".zip") else "download.tgz"
    )
    destination.mkdir(parents=True, exist_ok=True)
    download(distribution, archive_path)
    library_path = destination / distribution.library_file
    extract_member(archive_path, distribution.archive_member, library_path)
    extract_member(archive_path, distribution.license_member, destination / "LICENSE")
    archive_path.unlink()
    runtime_sha256 = sha256_file(library_path)
    (destination / "manifest.json").write_text(
        json.dumps(
            {"file": distribution.library_file, "sha256": runtime_sha256},
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )
    return destination


def download(distribution: RuntimeDistribution, destination: Path) -> None:
    digest = hashlib.sha256()
    with urllib.request.urlopen(distribution.url, timeout=120) as response:
        with open(destination, "wb") as archive:
            while chunk := response.read(1024 * 1024):
                digest.update(chunk)
                archive.write(chunk)
    if digest.hexdigest() != distribution.archive_sha256:
        destination.unlink(missing_ok=True)
        raise RuntimeError("Invalid ONNX Runtime download checksum")


def extract_member(archive_path: Path, member: str, destination: Path) -> None:
    if archive_path.suffix == ".zip":
        with zipfile.ZipFile(archive_path) as archive:
            with archive.open(member) as source, open(destination, "wb") as output:
                shutil.copyfileobj(source, output)
        return
    with tarfile.open(archive_path, mode="r:gz") as archive:
        source = archive.extractfile(member)
        if source is None:
            raise RuntimeError("Pinned ONNX Runtime archive is missing its library")
        with source, open(destination, "wb") as output:
            shutil.copyfileobj(source, output)


def sha256_file(path: Path) -> str:
    with open(path, "rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()
