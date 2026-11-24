"""Validate completed release artifacts before resuming a release build."""

import hashlib
import json
from pathlib import Path
import plistlib
import subprocess
import tarfile
import tempfile
import zipfile

from xedoc_package.archive import resolve_zstd_command
from xedoc_package.layout import validate_package_dir
from xedoc_package.model_router_runtime import (
    RuntimeReference,
    runtime_asset_name,
    runtime_id,
)
from xedoc_package.remote_agent_runtime import (
    RemoteAgentRuntimeReference,
    load_dependency_lock,
)
from xedoc_package.remote_agent_payload import load_manifest
from xedoc_package.targets import PACKAGE_VARIANTS, TARGET_SPECS
from xedoc_desktop_release import desktop_asset_name


def resumable_package(
    *,
    target: str,
    version: str,
    package_dir: Path,
    archive_outputs: list[Path],
    runtime_archive_output: Path,
    remote_agent_runtime_output: Path,
    notarization_report_dir: Path,
    notarize: bool,
    signing_identity: str | None,
) -> RuntimeReference | None:
    spec = TARGET_SPECS[target]
    required_paths = [package_dir, *archive_outputs, remote_agent_runtime_output]
    if not any(path.exists() for path in required_paths):
        return None
    if not all(path.exists() for path in required_paths):
        raise RuntimeError(
            f"Cannot resume incomplete release outputs for {target}; "
            "use --force to rebuild this target."
        )
    try:
        metadata = json.loads(
            (package_dir / "xedoc-package.json").read_text(encoding="utf-8")
        )
        runtime = metadata["modelRouterRuntime"]
        remote = metadata["remoteAgent"]
        if metadata.get("version") != version or metadata.get("target") != target:
            raise ValueError("package version or target does not match")
        if not isinstance(runtime, dict) or not isinstance(remote, dict):
            raise ValueError("runtime metadata is missing")
        runtime_reference = RuntimeReference(
            runtime_id=runtime["runtimeId"],
            asset_name=runtime["assetName"],
            sha256=runtime["sha256"],
            source_release_tag=runtime["sourceReleaseTag"],
        )
        expected_asset_name = runtime_asset_name(runtime_reference.runtime_id, target)
        if (
            runtime_reference.runtime_id != runtime_id(target, signing_identity)
            or runtime_reference.asset_name != expected_asset_name
            or runtime_archive_output.name != expected_asset_name
        ):
            raise ValueError("model-router runtime identity or checksum does not match")
        if not runtime_archive_output.is_file() or (
            sha256_file(runtime_archive_output) != runtime_reference.sha256
        ):
            raise ValueError("model-router runtime archive is missing or invalid")
        dependency_lock = load_dependency_lock(spec)
        manifest = load_manifest(
            package_dir / "xedoc-resources/remote-agent/remote-agent-manifest.json"
        )
        remote_runtime = manifest["runtime"]
        remote_reference = RemoteAgentRuntimeReference(
            target=target,
            runtime_id=remote_runtime["runtimeId"],
            python_version=remote_runtime["pythonVersion"],
            runtime_sha256=remote_runtime["sha256"],
            dependency_lock_sha256=dependency_lock.sha256,
            dependencies=dependency_lock.dependencies,
            site_packages=remote_runtime["sitePackages"],
            root=remote_agent_runtime_output / "python",
        )
        validate_package_dir(
            package_dir,
            PACKAGE_VARIANTS["xedoc"],
            spec,
            model_router_runtime=runtime_reference,
            remote_agent_runtime=remote_reference,
            include_session_control=True,
        )
        if notarize and target.endswith("apple-darwin"):
            notarization_zip = next(
                (path for path in archive_outputs if path.suffix == ".zip"), None
            )
            expected_name = (
                notarization_zip.name
                if notarization_zip is not None
                else "xedoc-package.zip"
            )
            summary = (
                notarization_report_dir
                / target
                / f"{expected_name}-notarization-summary.txt"
            )
            if (
                not summary.is_file()
                or "notarization=completed"
                not in summary.read_text(encoding="utf-8").splitlines()
            ):
                raise ValueError("notarization is not complete")
        for archive_output in archive_outputs:
            validate_package_archive(
                archive_output, package_dir, spec=spec, version=version
            )
    except (
        KeyError,
        OSError,
        TypeError,
        ValueError,
        EOFError,
        json.JSONDecodeError,
        RuntimeError,
        tarfile.TarError,
        zipfile.BadZipFile,
    ) as error:
        raise RuntimeError(
            f"Cannot resume invalid release outputs for {target}; "
            "use --force to rebuild this target."
        ) from error
    return runtime_reference


def validate_package_archive(
    archive_path: Path, package_dir: Path, *, spec, version: str
) -> None:
    if archive_path.suffix == ".zip":
        with zipfile.ZipFile(archive_path) as archive:
            expected_files = package_file_names(package_dir)
            members = {
                info.filename: info for info in archive.infolist() if not info.is_dir()
            }
            if set(members) != expected_files:
                raise ValueError("Release archive file list does not match package")
            for name, info in members.items():
                if info.file_size != (package_dir / name).stat().st_size:
                    raise ValueError(f"Release archive file size mismatch: {name}")
                if archive.read(name) != (package_dir / name).read_bytes():
                    raise ValueError(
                        f"Release archive file does not match package: {name}"
                    )
            validate_package_metadata(
                archive.read, package_dir, spec=spec, version=version
            )
    elif archive_path.suffix in {".gz", ".tgz"}:
        with tarfile.open(archive_path, "r|gz") as archive:
            validate_tar_stream(archive, package_dir, spec=spec, version=version)
    elif archive_path.name.endswith(".tar.zst"):
        with tempfile.TemporaryDirectory(prefix="xedoc-resume-archive-") as temp_dir:
            tar_path = Path(temp_dir) / "package.tar"
            with tar_path.open("wb") as output:
                subprocess.run(
                    [*resolve_zstd_command(), "-d", "-c", str(archive_path)],
                    check=True,
                    stdout=output,
                )
            with tarfile.open(tar_path, "r:") as archive:
                validate_tar_stream(archive, package_dir, spec=spec, version=version)
    else:
        raise ValueError(f"Unsupported release archive: {archive_path}")


def package_file_names(package_dir: Path) -> set[str]:
    return {
        path.relative_to(package_dir).as_posix()
        for path in package_dir.rglob("*")
        if path.is_file()
    }


def validate_tar_stream(
    archive: tarfile.TarFile,
    package_dir: Path,
    *,
    spec,
    version: str,
) -> None:
    expected_files = package_file_names(package_dir)
    archive_files = set()
    metadata_bytes = None
    for member in archive:
        if not member.isfile():
            continue
        archive_files.add(member.name)
        if member.name not in expected_files:
            raise ValueError("Release archive contains unexpected package files")
        if member.size != (package_dir / member.name).stat().st_size:
            raise ValueError(f"Release archive file size mismatch: {member.name}")
        source = archive.extractfile(member)
        if source is None:
            raise ValueError(f"Cannot read release archive file: {member.name}")
        archived_bytes = source.read()
        expected_bytes = (package_dir / member.name).read_bytes()
        if archived_bytes != expected_bytes:
            raise ValueError(
                f"Release archive file does not match package: {member.name}"
            )
        if member.name == "xedoc-package.json":
            metadata_bytes = archived_bytes
    if archive_files != expected_files or metadata_bytes is None:
        raise ValueError("Release archive file list does not match package")
    metadata = json.loads(metadata_bytes)
    if metadata.get("version") != version or metadata.get("target") != spec.target:
        raise ValueError("Release archive metadata version or target does not match")


def validate_package_metadata(
    read_entry, package_dir: Path, *, spec, version: str
) -> None:
    metadata = json.loads(read_entry("xedoc-package.json"))
    expected = json.loads(
        (package_dir / "xedoc-package.json").read_text(encoding="utf-8")
    )
    if metadata != expected:
        raise ValueError("Release archive package metadata does not match")
    if metadata.get("version") != version or metadata.get("target") != spec.target:
        raise ValueError("Release archive metadata version or target does not match")


def validate_app_archive(
    archive_path: Path, *, version: str, info_plist_path: str
) -> None:
    with zipfile.ZipFile(archive_path) as archive:
        if archive.testzip() is not None:
            raise ValueError(f"Corrupt release archive: {archive_path}")
        names = {name.rstrip("/") for name in archive.namelist()}
        info = plistlib.loads(archive.read(info_plist_path))
        bundle_path = info_plist_path.rpartition("/")[0]
        executable_name = info.get("CFBundleExecutable")
        executable_path = (
            f"{bundle_path}/MacOS/{executable_name}"
            if bundle_path.endswith("/Contents")
            else f"{bundle_path}/{executable_name}"
        )
        if (
            not isinstance(executable_name, str)
            or executable_path not in names
            or len(names) < 2
        ):
            raise ValueError("Release app archive is missing its executable")
        if not archive.read(executable_path):
            raise ValueError("Release app executable is empty")
    if info.get("CFBundleShortVersionString") != version:
        raise ValueError(
            f"Release archive version mismatch: expected {version}, "
            f"found {info.get('CFBundleShortVersionString')}"
        )


def validate_desktop_archive(
    archive_path: Path, *, version: str, platform_arch: str, extension: str
) -> None:
    if archive_path.name != desktop_asset_name(version, platform_arch, extension):
        raise ValueError(f"Unexpected desktop archive name: {archive_path}")
    size = archive_path.stat().st_size
    with archive_path.open("rb") as archive:
        header = archive.read(8)
        if extension == "deb" and header == b"!<arch>\n":
            archive.seek(8)
            members = []
            while archive.tell() + 60 <= size:
                member = archive.read(60)
                if member[58:60] != b"`\n":
                    raise ValueError(f"Invalid Debian archive member: {archive_path}")
                try:
                    member_size = int(member[48:58].strip())
                except ValueError as error:
                    raise ValueError(
                        f"Invalid Debian archive member size: {archive_path}"
                    ) from error
                members.append(
                    member[0:16].decode("ascii", "replace").strip().rstrip("/")
                )
                archive.seek(member_size + member_size % 2, 1)
            if (
                archive.tell() != size
                or "debian-binary" not in members
                or not any(name.startswith("control.tar") for name in members)
                or not any(name.startswith("data.tar") for name in members)
            ):
                raise ValueError(f"Incomplete Debian archive: {archive_path}")
        elif extension == "AppImage" and (
            not header.startswith(b"\x7fELF") or size < 1024 * 1024
        ):
            raise ValueError(f"Incomplete AppImage: {archive_path}")
        elif extension == "exe" and (
            not header.startswith(b"MZ") or size < 1024 * 1024
        ):
            raise ValueError(f"Incomplete Windows installer: {archive_path}")
    if extension == "AppImage" and not header.startswith(b"\x7fELF"):
        raise ValueError(f"Invalid AppImage header: {archive_path}")
    if extension == "deb" and header != b"!<arch>\n":
        raise ValueError(f"Invalid Debian archive header: {archive_path}")
    if extension == "exe" and not header.startswith(b"MZ"):
        raise ValueError(f"Invalid Windows installer header: {archive_path}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()
