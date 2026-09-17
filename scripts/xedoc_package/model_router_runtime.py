"""Build immutable Python runtimes for semantic model routing."""

import hashlib
import json
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
import zipfile
from dataclasses import asdict
from dataclasses import dataclass
from pathlib import Path

from .targets import TargetSpec


RUNTIME_SERIES = "r1"
RUNTIME_MANIFEST = "model-router-runtime.json"
PYTHON_RELEASE = "20260814"
PYTHON_VERSION = "3.12.14"
MODEL_NAME = "snowflake-arctic-embed-xs"
MODEL_BASE_URL = f"https://huggingface.co/Snowflake/{MODEL_NAME}/resolve/main"
SEMANTIC_POLICY_SOURCE = (
    Path(__file__).resolve().parents[1]
    / "model-router"
    / "reference-router.semantic-policy.json"
)
SEMANTIC_POLICY_PATH = Path("classifier") / SEMANTIC_POLICY_SOURCE.name
WHEEL_LOCK_SOURCE = Path(__file__).with_name("model_router_runtime_wheels.json")
RUNTIME_REQUIREMENTS = (
    "coloredlogs==15.0.1",
    "flatbuffers==25.12.19",
    "humanfriendly==10.0",
    "mpmath==1.3.0",
    "numpy==2.3.5",
    "packaging==26.3",
    "protobuf==7.36.1",
    "sympy==1.14.0",
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


@dataclass(frozen=True)
class RuntimeDistribution:
    python_url: str
    python_sha256: str
    pip_platform: str
    onnxruntime_version: str


@dataclass(frozen=True)
class RuntimeReference:
    runtime_id: str
    asset_name: str
    sha256: str
    source_release_tag: str

    def package_metadata(self) -> dict[str, str]:
        return {
            "runtimeId": self.runtime_id,
            "assetName": self.asset_name,
            "sha256": self.sha256,
            "sourceReleaseTag": self.source_release_tag,
        }


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
        "macosx_11_0_x86_64",
        "1.19.2",
    ),
    "aarch64-unknown-linux-gnu": python_distribution(
        "aarch64-unknown-linux-gnu",
        "2d8e17dfd732102cfeb18e0e1fa6769b24caa034e159981129590fe409c7157a",
        "manylinux_2_27_aarch64",
        "1.19.2",
    ),
    "x86_64-unknown-linux-gnu": python_distribution(
        "x86_64-unknown-linux-gnu",
        "5acfa3e9ba26b51ae161c83aff278da915b590d22373a424b2ba55b8afe91fcc",
        "manylinux_2_27_x86_64",
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


def runtime_id() -> str:
    """Return the logical runtime identity shared by every supported target."""
    digest = hashlib.sha256(
        json.dumps(
            runtime_identity_inputs(),
            sort_keys=True,
            separators=(",", ":"),
        ).encode()
    ).hexdigest()
    return f"{RUNTIME_SERIES}-sha256-{digest}"


def runtime_identity_inputs() -> dict[str, object]:
    """Return every immutable input that defines a router runtime revision."""
    if not SEMANTIC_POLICY_SOURCE.is_file():
        raise RuntimeError(
            f"Missing semantic classifier weights: {SEMANTIC_POLICY_SOURCE}"
        )
    return {
        "runtimeSeries": RUNTIME_SERIES,
        "python": {
            "release": PYTHON_RELEASE,
            "version": PYTHON_VERSION,
            "distributions": {
                target: asdict(distribution)
                for target, distribution in sorted(RUNTIME_DISTRIBUTIONS.items())
            },
        },
        "wheels": load_wheel_lock(),
        "model": {
            "name": MODEL_NAME,
            "files": MODEL_FILES,
        },
        "classifierWeights": {
            "path": SEMANTIC_POLICY_PATH.as_posix(),
            "sha256": sha256_file(SEMANTIC_POLICY_SOURCE),
        },
    }


def runtime_release_tag(runtime_id_value: str) -> str:
    return f"model-router-runtime-{runtime_id_value}"


def runtime_asset_name(runtime_id_value: str, target: str) -> str:
    return f"xedoc-model-router-runtime-{target}-{runtime_id_value}.zip"


def runtime_reference(spec: TargetSpec, archive_path: Path) -> RuntimeReference:
    runtime_id_value = runtime_id()
    return RuntimeReference(
        runtime_id=runtime_id_value,
        asset_name=runtime_asset_name(runtime_id_value, spec.target),
        sha256=sha256_file(archive_path),
        source_release_tag=runtime_release_tag(runtime_id_value),
    )


def build_runtime_archive(
    spec: TargetSpec,
    destination: Path,
    *,
    force: bool,
) -> RuntimeReference:
    """Build one reproducible, checksummed target runtime archive."""
    try:
        distribution = RUNTIME_DISTRIBUTIONS[spec.target]
    except KeyError as error:
        raise RuntimeError(
            f"No semantic model-router runtime is available for {spec.target}."
        ) from error

    runtime_id_value = runtime_id()
    expected_asset_name = runtime_asset_name(runtime_id_value, spec.target)
    if destination.name != expected_asset_name:
        raise RuntimeError(
            "Runtime archive name must match its immutable identity: "
            f"expected {expected_asset_name}, got {destination.name}"
        )

    with tempfile.TemporaryDirectory(prefix="xedoc-model-router-runtime-") as temp:
        root = Path(temp) / "runtime"
        root.mkdir()
        archive = Path(temp) / "python.tar.gz"
        download_file(distribution.python_url, archive, distribution.python_sha256)
        extract_tar(archive, root)
        wheel_set = install_wheels(
            root=root,
            target=spec.target,
            distribution=distribution,
            temp_dir=Path(temp),
        )
        download_model(root / "arctic-embed-xs")
        copy_classifier_weights(root)
        write_manifest(
            root,
            spec.target,
            runtime_id_value,
            distribution,
            wheel_set,
        )
        write_runtime_archive(root, destination, force=force)

    return runtime_reference(spec, destination)


def install_wheels(
    *,
    root: Path,
    target: str,
    distribution: RuntimeDistribution,
    temp_dir: Path,
) -> list[dict[str, str]]:
    wheels_dir = temp_dir / "wheels"
    wheels_dir.mkdir()
    requirements = [
        f"onnxruntime=={distribution.onnxruntime_version}",
        *RUNTIME_REQUIREMENTS,
    ]
    subprocess.run(
        [
            sys.executable,
            "-m",
            "pip",
            "download",
            "--only-binary=:all:",
            "--no-deps",
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
    wheel_set = validate_wheels(wheels_dir, target)
    site_packages = root / "site-packages"
    site_packages.mkdir()
    for wheel in sorted(wheels_dir.glob("*.whl")):
        with zipfile.ZipFile(wheel) as archive:
            extract_zip(archive, site_packages)
    return wheel_set


def load_wheel_lock() -> dict[str, dict[str, str]]:
    """Load the exact target wheel artifacts that define this runtime revision."""
    try:
        contents = json.loads(WHEEL_LOCK_SOURCE.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise RuntimeError(
            f"Could not load model-router wheel lock: {WHEEL_LOCK_SOURCE}"
        ) from error
    if not isinstance(contents, dict) or set(contents) != set(RUNTIME_DISTRIBUTIONS):
        raise RuntimeError("Model-router wheel lock must cover every supported target.")
    lock: dict[str, dict[str, str]] = {}
    for target, wheel_hashes in contents.items():
        if (
            not isinstance(wheel_hashes, dict)
            or not wheel_hashes
            or any(
                not isinstance(name, str)
                or not name.endswith(".whl")
                or not isinstance(checksum, str)
                or len(checksum) != 64
                or any(character not in "0123456789abcdef" for character in checksum)
                for name, checksum in wheel_hashes.items()
            )
        ):
            raise RuntimeError(
                f"Model-router wheel lock has invalid artifacts for {target}."
            )
        lock[target] = dict(sorted(wheel_hashes.items()))
    return dict(sorted(lock.items()))


def validate_wheels(wheels_dir: Path, target: str) -> list[dict[str, str]]:
    expected = load_wheel_lock()[target]
    actual = {
        wheel.name: sha256_file(wheel) for wheel in sorted(wheels_dir.glob("*.whl"))
    }
    if actual != expected:
        missing = sorted(set(expected).difference(actual))
        unexpected = sorted(set(actual).difference(expected))
        changed = sorted(
            name
            for name in set(expected).intersection(actual)
            if expected[name] != actual[name]
        )
        raise RuntimeError(
            f"Model-router runtime wheels for {target} differ from the checked-in "
            f"lock (missing={missing}, unexpected={unexpected}, changed={changed})."
        )
    return [
        {"assetName": name, "sha256": checksum} for name, checksum in expected.items()
    ]


def download_model(destination: Path) -> None:
    for relative_path, expected_sha256 in MODEL_FILES.items():
        destination_path = destination / relative_path
        destination_path.parent.mkdir(parents=True, exist_ok=True)
        download_file(
            f"{MODEL_BASE_URL}/{relative_path}",
            destination_path,
            expected_sha256,
        )


def copy_classifier_weights(root: Path) -> None:
    destination = root / SEMANTIC_POLICY_PATH
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(SEMANTIC_POLICY_SOURCE.read_bytes())


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


def write_manifest(
    root: Path,
    target: str,
    runtime_id_value: str,
    distribution: RuntimeDistribution,
    wheel_set: list[dict[str, str]],
) -> None:
    checksums = {
        path.relative_to(root).as_posix(): sha256_file(path)
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }
    (root / RUNTIME_MANIFEST).write_text(
        json.dumps(
            {
                "runtimeId": runtime_id_value,
                "target": target,
                "pythonDistribution": {
                    "release": PYTHON_RELEASE,
                    "version": PYTHON_VERSION,
                    "url": distribution.python_url,
                    "sha256": distribution.python_sha256,
                },
                "wheelSet": wheel_set,
                "installedDistributions": installed_wheels(root),
                "model": {
                    "name": MODEL_NAME,
                    "files": MODEL_FILES,
                },
                "classifierWeights": {
                    "path": SEMANTIC_POLICY_PATH.as_posix(),
                    "sha256": sha256_file(root / SEMANTIC_POLICY_PATH),
                },
                "files": checksums,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )


def installed_wheels(root: Path) -> list[dict[str, str]]:
    wheel_set: list[dict[str, str]] = []
    for metadata in sorted((root / "site-packages").glob("*.dist-info/METADATA")):
        name = ""
        version = ""
        for line in metadata.read_text(encoding="utf-8").splitlines():
            if line.startswith("Name: "):
                name = line.removeprefix("Name: ")
            elif line.startswith("Version: "):
                version = line.removeprefix("Version: ")
            if name and version:
                wheel_set.append({"name": name, "version": version})
                break
    return wheel_set


def write_runtime_archive(root: Path, destination: Path, *, force: bool) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists():
        if not force:
            raise RuntimeError(f"Runtime archive output already exists: {destination}")
        destination.unlink()

    with zipfile.ZipFile(
        destination,
        "w",
        compression=zipfile.ZIP_DEFLATED,
        compresslevel=9,
    ) as archive:
        for path in sorted(root.rglob("*")):
            if not path.is_file():
                continue
            relative_path = path.relative_to(root).as_posix()
            info = zipfile.ZipInfo(relative_path, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = (path.stat().st_mode & 0o777) << 16
            archive.writestr(info, path.read_bytes())


def sha256_file(path: Path) -> str:
    with open(path, "rb") as file:
        return hashlib.file_digest(file, "sha256").hexdigest()
