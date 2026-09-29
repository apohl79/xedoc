"""Pinned Python runtime and dependency closure for the remote-agent payload."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
from pathlib import PurePosixPath
import shutil
import stat
import tarfile
import tempfile
from typing import Any
from typing import Iterable
from urllib.parse import urlparse
import urllib.request
import zipfile

from .targets import TargetSpec


REMOTE_AGENT_RUNTIME_SERIES = "remote-agent-python-r2"
PYTHON_RELEASE = "20260814"
PYTHON_VERSION = "3.12.14"
DEPENDENCY_LOCK_VERSION = 1
DEPENDENCY_LOCK_PATH = (
    Path(__file__).resolve().parents[2] / "remote-agent" / "dependency-lock.json"
)
DOWNLOAD_TIMEOUT_SECONDS = 300
MAX_WHEEL_BYTES = 64 * 1024 * 1024
MAX_WHEEL_ENTRIES = 16 * 1024
MAX_WHEEL_UNPACKED_BYTES = 128 * 1024 * 1024
DEPENDENCY_HOST = "files.pythonhosted.org"


@dataclass(frozen=True)
class PythonDistribution:
    target: str
    url: str
    sha256: str


@dataclass(frozen=True)
class DependencyArtifact:
    name: str
    version: str
    filename: str
    url: str
    sha256: str

    @property
    def identity(self) -> str:
        return f"{self.name}=={self.version}"


@dataclass(frozen=True)
class DependencyLock:
    sha256: str
    dependencies: tuple[str, ...]
    artifacts: tuple[DependencyArtifact, ...]


def _distribution(target: str, sha256: str) -> PythonDistribution:
    filename = (
        f"cpython-{PYTHON_VERSION}+{PYTHON_RELEASE}-{target}"
        "-install_only_stripped.tar.gz"
    )
    return PythonDistribution(
        target=target,
        url=(
            "https://github.com/astral-sh/python-build-standalone/releases/download/"
            f"{PYTHON_RELEASE}/{filename.replace('+', '%2B')}"
        ),
        sha256=sha256,
    )


# The archive and checksum are release inputs, not runtime downloads. Keeping
# one entry for every package target makes a release independent of the host
# Python installation and prevents a target from silently using another ABI.
PYTHON_DISTRIBUTIONS = {
    "aarch64-apple-darwin": _distribution(
        "aarch64-apple-darwin",
        "dd5b76ab11451a4a4367c17c61d944dded56b425396b07f102922a7ebef7d55f",
    ),
    "x86_64-apple-darwin": _distribution(
        "x86_64-apple-darwin",
        "aec265e3cddaccdb2a3d783331596351b24d4a63c97af0a38f75f643c9451de9",
    ),
    "aarch64-unknown-linux-gnu": _distribution(
        "aarch64-unknown-linux-gnu",
        "2d8e17dfd732102cfeb18e0e1fa6769b24caa034e159981129590fe409c7157a",
    ),
    "x86_64-unknown-linux-gnu": _distribution(
        "x86_64-unknown-linux-gnu",
        "5acfa3e9ba26b51ae161c83aff278da915b590d22373a424b2ba55b8afe91fcc",
    ),
    "aarch64-pc-windows-msvc": _distribution(
        "aarch64-pc-windows-msvc",
        "1e1de8b5d0df73b965aa72f0c27d5c617a5d7256ce6d205228a0f9638bf6df21",
    ),
    "x86_64-pc-windows-msvc": _distribution(
        "x86_64-pc-windows-msvc",
        "89f18f6932917163b74339ebcec2645c8e47ae7f1c5f2ac37f2b4f4cf3beb647",
    ),
}


@dataclass(frozen=True)
class RemoteAgentRuntimeReference:
    """Identity and package-relative location of one copied runtime."""

    target: str
    runtime_id: str
    python_version: str
    runtime_sha256: str
    dependency_lock_sha256: str
    dependencies: tuple[str, ...]
    site_packages: str
    root: Path

    def package_metadata(self) -> dict[str, object]:
        return {
            "target": self.target,
            "runtimeId": self.runtime_id,
            "pythonVersion": self.python_version,
            "sha256": self.runtime_sha256,
            "path": "xedoc-resources/remote-agent/runtime/python",
            "sitePackages": self.site_packages,
            "dependencyLockSha256": self.dependency_lock_sha256,
            "dependencies": list(self.dependencies),
        }


def build_remote_agent_runtime(
    spec: TargetSpec,
    destination: Path,
    *,
    force: bool,
    source: Path | None = None,
) -> RemoteAgentRuntimeReference:
    """Build a target-pinned runtime and dependency closure into ``destination``.

    ``source`` is used by release builders that pre-provision a runtime. It is
    still validated and receives only the checked-in, target-specific wheel
    closure. Without it, the target runtime is downloaded and verified once
    during release creation. The installed package never downloads or resolves
    Python dependencies.
    """

    if spec.is_windows:
        raise RuntimeError(
            f"Remote-agent Python runtime is unsupported on Windows target {spec.target}."
        )
    distribution = PYTHON_DISTRIBUTIONS.get(spec.target)
    if distribution is None:
        raise RuntimeError(
            f"No remote-agent Python runtime is available for {spec.target}."
        )
    dependency_lock = load_dependency_lock(spec)

    with tempfile.TemporaryDirectory(prefix="xedoc-remote-agent-runtime-") as temp:
        staging = Path(temp) / "runtime"
        python_root = staging / "python"
        if source is None:
            archive = Path(temp) / "python.tar.gz"
            _download_file(distribution.url, archive, distribution.sha256)
            extracted = Path(temp) / "extracted"
            extracted.mkdir()
            _extract_tar(archive, extracted)
            source_root = _find_python_root(extracted, spec)
        else:
            source_root = _find_python_root(source.resolve(), spec)
        _copy_runtime(source_root, python_root)
        site_packages = runtime_site_packages_path(spec)
        dependency_destination = python_root / site_packages
        _install_dependencies(
            dependency_lock.artifacts,
            dependency_destination,
            Path(temp) / "wheels",
        )
        _validate_runtime(python_root, spec, dependency_lock)
        runtime_sha256 = hash_tree(python_root)
        runtime_id = runtime_identity(
            spec, distribution, dependency_lock, runtime_sha256
        )
        _install_runtime(staging, destination, force=force)

    return RemoteAgentRuntimeReference(
        target=spec.target,
        runtime_id=runtime_id,
        python_version=PYTHON_VERSION,
        runtime_sha256=runtime_sha256,
        dependency_lock_sha256=dependency_lock.sha256,
        dependencies=dependency_lock.dependencies,
        site_packages=(
            Path("runtime") / "python" / runtime_site_packages_path(spec)
        ).as_posix(),
        root=(destination / "python").resolve(),
    )


def load_dependency_lock(spec: TargetSpec) -> DependencyLock:
    """Load only the exact wheel closure declared for ``spec``."""

    try:
        raw = DEPENDENCY_LOCK_PATH.read_bytes()
        value = json.loads(raw)
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise RuntimeError(
            f"Remote-agent dependency lock is invalid: {DEPENDENCY_LOCK_PATH}"
        ) from error
    if not isinstance(value, dict) or set(value) != {
        "dependencies",
        "lockVersion",
        "pythonVersion",
        "targets",
    }:
        raise RuntimeError("Remote-agent dependency lock has invalid fields")
    if value["lockVersion"] != DEPENDENCY_LOCK_VERSION or value[
        "pythonVersion"
    ] != ".".join(PYTHON_VERSION.split(".")[:2]):
        raise RuntimeError("Remote-agent dependency lock has an incompatible version")
    dependencies = _dependency_identities(value["dependencies"])
    targets = value["targets"]
    if not isinstance(targets, dict):
        raise RuntimeError("Remote-agent dependency lock targets are invalid")
    target_artifacts = targets.get(spec.target)
    if not isinstance(target_artifacts, list):
        raise RuntimeError(
            "Remote-agent dependency lock has no compatible wheel closure for "
            f"{spec.target}"
        )
    artifacts = tuple(_artifact(item) for item in target_artifacts)
    if (
        len(artifacts) != len(dependencies)
        or tuple(artifact.identity for artifact in artifacts) != dependencies
        or len({artifact.filename for artifact in artifacts}) != len(artifacts)
    ):
        raise RuntimeError("Remote-agent dependency lock target closure is invalid")
    return DependencyLock(
        sha256=hashlib.sha256(raw).hexdigest(),
        dependencies=dependencies,
        artifacts=artifacts,
    )


def runtime_identity(
    spec: TargetSpec,
    distribution: PythonDistribution,
    dependency_lock: DependencyLock,
    runtime_sha256: str,
) -> str:
    value = {
        "series": REMOTE_AGENT_RUNTIME_SERIES,
        "target": spec.target,
        "pythonRelease": PYTHON_RELEASE,
        "pythonVersion": PYTHON_VERSION,
        "distributionSha256": distribution.sha256,
        "dependencyLockSha256": dependency_lock.sha256,
        "dependencies": dependency_lock.dependencies,
        "runtimeSha256": runtime_sha256,
    }
    digest = hashlib.sha256(
        json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    return f"{REMOTE_AGENT_RUNTIME_SERIES}-{digest}"


def runtime_interpreter_path(root: Path, spec: TargetSpec) -> Path:
    relative = Path("python.exe") if spec.is_windows else Path("bin") / "python3"
    return root / relative


def runtime_site_packages_path(spec: TargetSpec) -> Path:
    if spec.is_windows:
        return Path("Lib") / "site-packages"
    return Path("lib") / f"python{PYTHON_VERSION.rsplit('.', 1)[0]}" / "site-packages"


def hash_tree(root: Path) -> str:
    """Hash a runtime tree by sorted relative names and file contents."""

    digest = hashlib.sha256()
    entries = sorted(_tree_entries(root), key=lambda item: item[0])
    for relative, path, link_target in entries:
        digest.update(relative.encode("utf-8"))
        digest.update(b"\0")
        if link_target is not None:
            digest.update(b"symlink\0")
            digest.update(link_target.encode("utf-8"))
        else:
            digest.update(b"file\0")
            with path.open("rb") as file:
                while chunk := file.read(1024 * 1024):
                    digest.update(chunk)
        digest.update(b"\0")
    return digest.hexdigest()


def _dependency_identities(value: object) -> tuple[str, ...]:
    if not isinstance(value, list) or not 0 < len(value) <= 16:
        raise RuntimeError("Remote-agent dependency lock dependencies are invalid")
    dependencies: list[str] = []
    for item in value:
        if not isinstance(item, dict) or set(item) != {"name", "version"}:
            raise RuntimeError("Remote-agent dependency lock dependency is invalid")
        name = _package_name(item["name"])
        version = _package_version(item["version"])
        dependencies.append(f"{name}=={version}")
    if dependencies != sorted(dependencies):
        raise RuntimeError("Remote-agent dependency lock dependencies are unordered")
    return tuple(dependencies)


def _artifact(value: object) -> DependencyArtifact:
    if not isinstance(value, dict) or set(value) != {
        "filename",
        "name",
        "sha256",
        "url",
        "version",
    }:
        raise RuntimeError("Remote-agent dependency wheel entry is invalid")
    name = _package_name(value["name"])
    version = _package_version(value["version"])
    filename = value["filename"]
    url = value["url"]
    sha256 = value["sha256"]
    if (
        not isinstance(filename, str)
        or not filename.endswith(".whl")
        or "/" in filename
        or "\\" in filename
        or len(filename) > 256
        or not isinstance(url, str)
        or not _is_expected_wheel_url(url, filename)
        or not isinstance(sha256, str)
        or len(sha256) != 64
        or any(character not in "0123456789abcdef" for character in sha256)
    ):
        raise RuntimeError("Remote-agent dependency wheel entry is invalid")
    return DependencyArtifact(name, version, filename, url, sha256)


def _package_name(value: object) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 64
        or any(
            character not in "abcdefghijklmnopqrstuvwxyz0123456789-"
            for character in value
        )
    ):
        raise RuntimeError("Remote-agent dependency name is invalid")
    return value


def _package_version(value: object) -> str:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > 64
        or any(
            character not in "abcdefghijklmnopqrstuvwxyz0123456789.-"
            for character in value
        )
    ):
        raise RuntimeError("Remote-agent dependency version is invalid")
    return value


def _is_expected_wheel_url(url: str, filename: str) -> bool:
    parsed = urlparse(url)
    return (
        parsed.scheme == "https"
        and parsed.hostname == DEPENDENCY_HOST
        and not parsed.username
        and not parsed.password
        and not parsed.query
        and not parsed.fragment
        and parsed.path.endswith(f"/{filename}")
    )


def _find_python_root(path: Path, spec: TargetSpec) -> Path:
    if runtime_interpreter_path(path, spec).is_file():
        return path
    candidate = path / "python"
    if runtime_interpreter_path(candidate, spec).is_file():
        return candidate
    raise RuntimeError(
        f"Remote-agent runtime for {spec.target} is missing its target "
        f"interpreter ({runtime_interpreter_path(path, spec)})."
    )


def _copy_runtime(source: Path, destination: Path) -> None:
    if destination.exists():
        shutil.rmtree(destination)
    shutil.copytree(source, destination, symlinks=True)


def _install_dependencies(
    artifacts: tuple[DependencyArtifact, ...],
    destination: Path,
    wheel_directory: Path,
) -> None:
    if destination.exists():
        if destination.is_symlink():
            raise RuntimeError("Remote-agent runtime site-packages is invalid")
        shutil.rmtree(destination)
    destination.mkdir(parents=True)
    wheel_directory.mkdir()
    unpacked: set[Path] = set()
    for artifact in artifacts:
        wheel = wheel_directory / artifact.filename
        _download_file(
            artifact.url,
            wheel,
            artifact.sha256,
            max_bytes=MAX_WHEEL_BYTES,
            expected_host=DEPENDENCY_HOST,
        )
        _extract_wheel(
            wheel,
            destination,
            artifact,
            unpacked,
        )


def _extract_wheel(
    wheel: Path,
    destination: Path,
    artifact: DependencyArtifact,
    unpacked: set[Path],
) -> None:
    try:
        with zipfile.ZipFile(wheel) as archive:
            infos = archive.infolist()
            if not 0 < len(infos) <= MAX_WHEEL_ENTRIES:
                raise RuntimeError("Remote-agent dependency wheel has invalid entries")
            _validate_wheel_metadata(archive, artifact)
            total = 0
            for info in infos:
                relative = _wheel_entry_path(info)
                if relative is None:
                    continue
                if relative in unpacked:
                    raise RuntimeError("Remote-agent dependency wheels overlap")
                total += info.file_size
                if total > MAX_WHEEL_UNPACKED_BYTES:
                    raise RuntimeError(
                        "Remote-agent dependency wheel exceeds its size bound"
                    )
                target = destination / relative
                if target.exists() or target.is_symlink():
                    raise RuntimeError(
                        "Remote-agent dependency wheel overwrites runtime data"
                    )
                target.parent.mkdir(parents=True, exist_ok=True)
                _write_wheel_file(archive, info, target)
                unpacked.add(relative)
    except (OSError, zipfile.BadZipFile) as error:
        raise RuntimeError(
            f"Remote-agent dependency wheel is invalid: {wheel.name}"
        ) from error


def _validate_wheel_metadata(
    archive: zipfile.ZipFile, artifact: DependencyArtifact
) -> None:
    prefix = f"{artifact.name.replace('-', '_')}-{artifact.version}.dist-info/"
    metadata_name = f"{prefix}METADATA"
    if metadata_name not in archive.namelist():
        raise RuntimeError("Remote-agent dependency wheel metadata is missing")
    try:
        metadata = archive.read(metadata_name).decode("utf-8")
    except (KeyError, UnicodeError) as error:
        raise RuntimeError(
            "Remote-agent dependency wheel metadata is invalid"
        ) from error
    fields = {
        line.partition(":")[0]: line.partition(":")[2].strip()
        for line in metadata.splitlines()
        if ":" in line
    }
    if (
        _normalize_distribution(fields.get("Name")) != artifact.name
        or fields.get("Version") != artifact.version
    ):
        raise RuntimeError("Remote-agent dependency wheel identity is invalid")


def _wheel_entry_path(info: zipfile.ZipInfo) -> Path | None:
    path = Path(info.filename)
    mode = info.external_attr >> 16
    if (
        path.is_absolute()
        or ".." in path.parts
        or not path.parts
        or stat.S_ISLNK(mode)
        or (info.is_dir() and info.filename.rstrip("/") != path.as_posix())
    ):
        raise RuntimeError("Remote-agent dependency wheel has unsafe entry")
    if info.is_dir():
        return None
    if info.file_size < 0 or info.compress_size < 0:
        raise RuntimeError("Remote-agent dependency wheel has invalid entry")
    return path


def _write_wheel_file(
    archive: zipfile.ZipFile, info: zipfile.ZipInfo, target: Path
) -> None:
    try:
        with archive.open(info) as source, target.open("xb") as output:
            copied = 0
            while chunk := source.read(1024 * 1024):
                copied += len(chunk)
                if copied > info.file_size:
                    raise RuntimeError("Remote-agent dependency wheel entry is invalid")
                output.write(chunk)
        if copied != info.file_size:
            raise RuntimeError("Remote-agent dependency wheel entry is truncated")
    except OSError as error:
        raise RuntimeError(
            "Remote-agent dependency wheel cannot be extracted"
        ) from error


def _normalize_distribution(value: object) -> str:
    if not isinstance(value, str):
        return ""
    return value.lower().replace("_", "-")


def _install_runtime(staging: Path, destination: Path, *, force: bool) -> None:
    destination = destination.resolve()
    if destination.exists():
        if not force:
            raise RuntimeError(
                f"Remote-agent runtime output already exists: {destination}"
            )
        shutil.rmtree(destination)
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(staging, destination, symlinks=True)


def _validate_runtime(
    root: Path, spec: TargetSpec, dependency_lock: DependencyLock
) -> None:
    interpreter = runtime_interpreter_path(root, spec)
    if not interpreter.is_file():
        raise RuntimeError(
            f"Remote-agent runtime interpreter is invalid: {interpreter}"
        )
    try:
        interpreter.resolve().relative_to(root.resolve())
    except ValueError as error:
        raise RuntimeError(
            f"Remote-agent runtime interpreter escapes its root: {interpreter}"
        ) from error
    if not spec.is_windows and not interpreter.stat().st_mode & stat.S_IXUSR:
        raise RuntimeError(
            f"Remote-agent runtime interpreter is not executable: {interpreter}"
        )
    root_resolved = root.resolve()
    for path in root.rglob("*"):
        if path.is_symlink():
            try:
                path.resolve().relative_to(root_resolved)
            except ValueError as error:
                raise RuntimeError(
                    f"Remote-agent runtime symlink escapes its root: {path}"
                ) from error
        elif not path.is_file() and not path.is_dir():
            raise RuntimeError(f"Remote-agent runtime has unsupported entry: {path}")
    site_packages = root / runtime_site_packages_path(spec)
    _validate_installed_dependencies(site_packages, dependency_lock.dependencies)


def _validate_installed_dependencies(
    site_packages: Path, dependencies: tuple[str, ...]
) -> None:
    if not site_packages.is_dir() or site_packages.is_symlink():
        raise RuntimeError("Remote-agent runtime site-packages is invalid")
    discovered: list[str] = []
    for path in site_packages.glob("*.dist-info"):
        if not path.is_dir() or path.is_symlink():
            raise RuntimeError("Remote-agent runtime dependency metadata is invalid")
        metadata = path / "METADATA"
        try:
            lines = metadata.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError) as error:
            raise RuntimeError(
                "Remote-agent runtime dependency metadata is unavailable"
            ) from error
        fields = {
            line.partition(":")[0]: line.partition(":")[2].strip()
            for line in lines
            if ":" in line
        }
        name = _normalize_distribution(fields.get("Name"))
        version = fields.get("Version")
        if not name or not isinstance(version, str):
            raise RuntimeError("Remote-agent runtime dependency metadata is invalid")
        discovered.append(f"{name}=={version}")
    if tuple(sorted(discovered)) != dependencies:
        raise RuntimeError("Remote-agent runtime dependency identity is invalid")


def _tree_entries(root: Path) -> Iterable[tuple[str, Path, str | None]]:
    for path in root.rglob("*"):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            yield relative, path, path.readlink().as_posix()
        elif path.is_file():
            yield relative, path, None


def _download_file(
    url: str,
    destination: Path,
    expected_sha256: str,
    *,
    max_bytes: int | None = None,
    expected_host: str | None = None,
) -> None:
    digest = hashlib.sha256()
    total = 0
    try:
        with urllib.request.urlopen(url, timeout=DOWNLOAD_TIMEOUT_SECONDS) as response:
            if expected_host is not None:
                actual = urlparse(response.geturl())
                if actual.scheme != "https" or actual.hostname != expected_host:
                    raise RuntimeError("Remote-agent dependency download redirected")
            with destination.open("xb") as output:
                while chunk := response.read(1024 * 1024):
                    total += len(chunk)
                    if max_bytes is not None and total > max_bytes:
                        raise RuntimeError(
                            "Remote-agent dependency download exceeds its size bound"
                        )
                    digest.update(chunk)
                    output.write(chunk)
    except RuntimeError:
        raise
    except OSError as error:
        raise RuntimeError(f"Could not download remote-agent runtime: {url}") from error
    actual = digest.hexdigest()
    if actual != expected_sha256:
        raise RuntimeError(
            f"Remote-agent runtime checksum mismatch: expected {expected_sha256}, got {actual}"
        )


def _extract_tar(archive_path: Path, destination: Path) -> None:
    with tarfile.open(archive_path, mode="r:gz") as archive:
        members: list[tarfile.TarInfo] = []
        names: set[str] = set()
        for member in archive.getmembers():
            _validate_member_path(member.name)
            if member.name in names:
                raise RuntimeError(
                    f"Remote-agent runtime archive has duplicate member: {member.name}"
                )
            names.add(member.name)
            if member.name.startswith("python/share/"):
                continue
            _validate_tar_member(member)
            members.append(member)
        try:
            archive.extractall(destination, members=members, filter="fully_trusted")
        except TypeError:
            archive.extractall(destination, members=members)


def _validate_member_path(name: str) -> None:
    path = Path(name)
    if path.is_absolute() or ".." in path.parts:
        raise RuntimeError(
            f"Remote-agent runtime archive has unsafe member path: {name}"
        )


def _validate_tar_member(member: tarfile.TarInfo) -> None:
    if member.isdir() or member.isfile():
        return
    if member.issym():
        _validate_tar_link(member.name, member.linkname, relative=True)
        return
    if member.islnk():
        _validate_tar_link(member.name, member.linkname, relative=False)
        return
    raise RuntimeError(
        f"Remote-agent runtime archive has unsupported entry: {member.name}"
    )


def _validate_tar_link(name: str, target: str, *, relative: bool) -> None:
    target_path = PurePosixPath(target)
    if not target or target_path.is_absolute():
        raise RuntimeError(f"Remote-agent runtime archive has unsafe link: {name}")
    base = PurePosixPath(name).parent if relative else PurePosixPath()
    parts: list[str] = []
    for part in (base / target_path).parts:
        if part in {"", "."}:
            continue
        if part == "..":
            if not parts:
                raise RuntimeError(
                    f"Remote-agent runtime archive has unsafe link: {name}"
                )
            parts.pop()
            continue
        parts.append(part)
