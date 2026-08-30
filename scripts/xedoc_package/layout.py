"""Canonical Xedoc package directory layout."""

import json
import shutil
import stat
from pathlib import Path

from .targets import PackageInputs
from .targets import PackageVariant
from .targets import TargetSpec

LAYOUT_VERSION = 1
SESSION_CONTROL_LAYOUT_VERSION = 2
SESSION_CONTROL_SOURCE = Path(__file__).resolve().parents[1] / "xedoc-session"
SESSION_CONTROL_NAME = "xedoc-session"


def prepare_package_dir(package_dir: Path, *, force: bool) -> None:
    if package_dir.exists():
        if not package_dir.is_dir():
            raise RuntimeError(
                f"Package output exists and is not a directory: {package_dir}"
            )
        if any(package_dir.iterdir()):
            if not force:
                raise RuntimeError(
                    f"Package output directory is not empty: {package_dir}. "
                    "Pass --force to replace it."
                )
            shutil.rmtree(package_dir)

    package_dir.mkdir(parents=True, exist_ok=True)


def build_package_dir(
    package_dir: Path,
    version: str,
    variant: PackageVariant,
    spec: TargetSpec,
    inputs: PackageInputs,
    *,
    include_session_control: bool = False,
) -> None:
    bin_dir = package_dir / "bin"
    resources_dir = package_dir / "xedoc-resources"
    path_dir = package_dir / "xedoc-path"
    bin_dir.mkdir()
    resources_dir.mkdir()
    path_dir.mkdir()

    entrypoint_name = variant.entrypoint_name(spec)
    copy_executable(
        inputs.entrypoint_bin,
        bin_dir / entrypoint_name,
        is_windows=spec.is_windows,
    )
    if include_session_control:
        if spec.is_windows:
            raise RuntimeError("Session-control CLI is only supported on Unix targets")
        if not SESSION_CONTROL_SOURCE.is_file():
            raise RuntimeError(
                f"Missing session-control script: {SESSION_CONTROL_SOURCE}"
            )
        copy_executable(
            SESSION_CONTROL_SOURCE,
            bin_dir / SESSION_CONTROL_NAME,
            is_windows=False,
        )
    copy_executable(inputs.rg_bin, path_dir / spec.rg_name, is_windows=spec.is_windows)

    if inputs.bwrap_bin is not None:
        copy_executable(inputs.bwrap_bin, resources_dir / "bwrap", is_windows=False)

    metadata = {
        "layoutVersion": (
            SESSION_CONTROL_LAYOUT_VERSION
            if include_session_control
            else LAYOUT_VERSION
        ),
        "version": version,
        "target": spec.target,
        "variant": variant.name,
        "entrypoint": f"bin/{entrypoint_name}",
        "resourcesDir": "xedoc-resources",
        "pathDir": "xedoc-path",
    }
    write_json(package_dir / "xedoc-package.json", metadata)


def validate_package_dir(
    package_dir: Path,
    variant: PackageVariant,
    spec: TargetSpec,
    *,
    include_session_control: bool = False,
) -> None:
    required_dirs = [
        Path("bin"),
        Path("xedoc-resources"),
        Path("xedoc-path"),
    ]
    for relative_dir in required_dirs:
        path = package_dir / relative_dir
        if not path.is_dir():
            raise RuntimeError(f"Missing package directory: {relative_dir}")

    metadata_path = package_dir / "xedoc-package.json"
    if not metadata_path.is_file():
        raise RuntimeError("Missing package metadata: xedoc-package.json")

    with open(metadata_path, encoding="utf-8") as fh:
        metadata = json.load(fh)

    expected_metadata = {
        "layoutVersion": (
            SESSION_CONTROL_LAYOUT_VERSION
            if include_session_control
            else LAYOUT_VERSION
        ),
        "target": spec.target,
        "variant": variant.name,
        "entrypoint": f"bin/{variant.entrypoint_name(spec)}",
        "resourcesDir": "xedoc-resources",
        "pathDir": "xedoc-path",
    }
    for key, expected in expected_metadata.items():
        actual = metadata.get(key)
        if actual != expected:
            raise RuntimeError(
                f"Invalid package metadata field {key!r}: expected {expected!r}, got {actual!r}"
            )

    required_files = [
        Path("bin") / variant.entrypoint_name(spec),
        Path("xedoc-path") / spec.rg_name,
    ]
    executable_files = list(required_files)

    if include_session_control:
        if spec.is_windows:
            raise RuntimeError("Session-control CLI is only supported on Unix targets")
        session_control_path = Path("bin") / SESSION_CONTROL_NAME
        required_files.append(session_control_path)
        executable_files.append(session_control_path)

    if spec.is_linux:
        required_files.append(Path("xedoc-resources") / "bwrap")
        executable_files.append(Path("xedoc-resources") / "bwrap")

    for relative_file in required_files:
        path = package_dir / relative_file
        if not path.is_file():
            raise RuntimeError(f"Missing package file: {relative_file}")

    if not spec.is_windows:
        for relative_file in executable_files:
            path = package_dir / relative_file
            if not is_executable(path):
                raise RuntimeError(f"Package file is not executable: {relative_file}")


def copy_executable(src: Path, dest: Path, *, is_windows: bool) -> None:
    dest.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(src, dest)
    if not is_windows:
        mode = dest.stat().st_mode
        dest.chmod(mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)


def write_json(path: Path, value: object) -> None:
    with open(path, "w", encoding="utf-8") as out:
        json.dump(value, out, indent=2)
        out.write("\n")


def is_executable(path: Path) -> bool:
    return bool(path.stat().st_mode & stat.S_IXUSR)
