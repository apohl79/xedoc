"""Build the separately installed Python runtime for semantic model routing."""

import hashlib
import json
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from dataclasses import dataclass
from pathlib import Path

from .archive import write_archive
from .targets import TargetSpec


PYTHON_RELEASE = "20260814"
PYTHON_VERSION = "3.12.14"
MODEL_BASE_URL = (
    "https://huggingface.co/Snowflake/snowflake-arctic-embed-xs/resolve/main"
)
MODEL_FILES = {
    "onnx/model.onnx": "cf2698d30ff05da02c70a088313bad56e5c2f401d734cb24a8390d446111936c",
    "tokenizer.json": "91f1def9b9391fdabe028cd3f3fcc4efd34e5d1f08c3bf2de513ebb5911a1854",  # gitleaks:allow -- immutable SHA-256.
    "config.json": "d7d071046ab952af96b7abad788db7ab3fc997b465e1b9914ff39707092254ec",
    "special_tokens_map.json": (
        "5d5b662e421ea9fac075174bb0688ee0d9431699900b90662acd44b2a350503a"
    ),
    "tokenizer_config.json": (
        "9ca59277519f6e3692c8685e26b94d4afca2d5438deff66483db495e48735810"
    ),
}
RUNTIME_MANIFEST = "model-router-runtime.json"


@dataclass(frozen=True)
class RuntimeDistribution:
    python_url: str
    python_sha256: str
    pip_platform: str
    onnxruntime_version: str


def python_distribution(
    target: str,
    sha256: str,
    pip_platform: str,
    onnxruntime_version: str,
) -> RuntimeDistribution:
    filename = (
        f"cpython-{PYTHON_VERSION}+{PYTHON_RELEASE}-{target}"
        "-install_only_stripped.tar.gz"
    )
    return RuntimeDistribution(
        python_url=(
            "https://github.com/astral-sh/python-build-standalone/releases/download/"
            f"{PYTHON_RELEASE}/{filename.replace('+', '%2B')}"
        ),
        python_sha256=sha256,
        pip_platform=pip_platform,
        onnxruntime_version=onnxruntime_version,
    )


RUNTIME_DISTRIBUTIONS: dict[str, RuntimeDistribution] = {
    "aarch64-apple-darwin": python_distribution(
        "aarch64-apple-darwin",
        "dd5b76ab11451a4a4367c17c61d944dded56b425396b07f102922a7ebef7d55f",
        "macosx_11_0_arm64",
        "1.19.2",
    ),
    "x86_64-apple-darwin": python_distribution(
        "x86_64-apple-darwin",
        "aec265e3cddaccdb2a3d783331596351b24d4a63c97af0a38f75f643c9451de9",
        "macosx_10_15_x86_64",
        "1.19.2",
    ),
    "aarch64-unknown-linux-gnu": python_distribution(
        "aarch64-unknown-linux-gnu",
        "2d8e17dfd732102cfeb18e0e1fa6769b24caa034e159981129590fe409c7157a",
        "manylinux_2_17_aarch64",
        "1.19.2",
    ),
    "x86_64-unknown-linux-gnu": python_distribution(
        "x86_64-unknown-linux-gnu",
        "5acfa3e9ba26b51ae161c83aff278da915b590d22373a424b2ba55b8afe91fcc",
        "manylinux_2_17_x86_64",
        "1.19.2",
    ),
    "aarch64-pc-windows-msvc": python_distribution(
        "aarch64-pc-windows-msvc",
        "1e1de8b5d0df73b965aa72f0c27d5c617a5d7256ce6d205228a0f9638bf6df21",
        "win_arm64",
        "1.30.0",
    ),
    "x86_64-pc-windows-msvc": python_distribution(
        "x86_64-pc-windows-msvc",
        "89f18f6932917163b74339ebcec2645c8e47ae7f1c5f2ac37f2b4f4cf3beb647",
        "win_amd64",
        "1.30.0",
    ),
}


def runtime_asset_name(version: str, target: str) -> str:
    return f"xedoc-model-router-runtime-{target}-{version}.zip"


def build_runtime_archive(
    spec: TargetSpec,
    version: str,
    destination: Path,
    *,
    force: bool,
) -> None:
    """Build one checksummed, target-specific semantic-router runtime archive."""
    try:
        distribution = RUNTIME_DISTRIBUTIONS[spec.target]
    except KeyError as error:
        raise RuntimeError(
            f"No semantic model-router runtime is available for {spec.target}."
        ) from error

    with tempfile.TemporaryDirectory(prefix="xedoc-model-router-runtime-") as temp:
        root = Path(temp) / "runtime"
        root.mkdir()
        archive = Path(temp) / "python.tar.gz"
        download_file(distribution.python_url, archive, distribution.python_sha256)
        extract_tar(archive, root)
        install_wheels(
            root=root,
            distribution=distribution,
            temp_dir=Path(temp),
        )
        download_model(root / "arctic-embed-xs")
        write_manifest(root, spec.target, version)
        write_archive(root, destination, force=force)


def install_wheels(
    *,
    root: Path,
    distribution: RuntimeDistribution,
    temp_dir: Path,
) -> None:
    wheels_dir = temp_dir / "wheels"
    wheels_dir.mkdir()
    requirements = [
        f"onnxruntime=={distribution.onnxruntime_version}",
        "tokenizers==0.23.2",
        "numpy==2.3.5",
    ]
    subprocess.run(
        [
            sys.executable,
            "-m",
            "pip",
            "download",
            "--only-binary=:all:",
            "--dest",
            str(wheels_dir),
            "--platform",
            distribution.pip_platform,
            "--implementation",
            "cp",
            "--python-version",
            "312",
            "--abi",
            "cp312",
            *requirements,
        ],
        check=True,
    )
    site_packages = root / "site-packages"
    site_packages.mkdir()
    for wheel in sorted(wheels_dir.glob("*.whl")):
        with zipfile.ZipFile(wheel) as archive:
            extract_zip(archive, site_packages)


def download_model(destination: Path) -> None:
    for relative_path, expected_sha256 in MODEL_FILES.items():
        destination_path = destination / relative_path
        destination_path.parent.mkdir(parents=True, exist_ok=True)
        download_file(
            f"{MODEL_BASE_URL}/{relative_path}",
            destination_path,
            expected_sha256,
        )


def download_file(url: str, destination: Path, expected_sha256: str) -> None:
    digest = hashlib.sha256()
    try:
        with urllib.request.urlopen(url, timeout=300) as response:
            with open(destination, "wb") as output:
                while chunk := response.read(1024 * 1024):
                    digest.update(chunk)
                    output.write(chunk)
    except OSError as error:
        raise RuntimeError(
            f"Could not download router runtime dependency: {url}"
        ) from error
    actual_sha256 = digest.hexdigest()
    if actual_sha256 != expected_sha256:
        raise RuntimeError(
            f"Router runtime dependency checksum mismatch for {url}: "
            f"expected {expected_sha256}, got {actual_sha256}"
        )


def extract_tar(archive_path: Path, destination: Path) -> None:
    with tarfile.open(archive_path, mode="r:gz") as archive:
        members = archive.getmembers()
        for member in members:
            validate_member_path(destination, member.name)
        archive.extractall(destination, members=members)


def extract_zip(archive: zipfile.ZipFile, destination: Path) -> None:
    for member in archive.infolist():
        validate_member_path(destination, member.filename)
    archive.extractall(destination)


def validate_member_path(destination: Path, name: str) -> None:
    path = Path(name)
    if path.is_absolute() or ".." in path.parts:
        raise RuntimeError(
            f"Router runtime archive has unsafe member path: {destination / path}"
        )


def write_manifest(root: Path, target: str, version: str) -> None:
    checksums = {
        path.relative_to(root).as_posix(): sha256_file(path)
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }
    (root / RUNTIME_MANIFEST).write_text(
        json.dumps(
            {
                "version": version,
                "target": target,
                "model": "snowflake-arctic-embed-xs",
                "python": PYTHON_VERSION,
                "files": checksums,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )


def sha256_file(path: Path) -> str:
    with open(path, "rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()
