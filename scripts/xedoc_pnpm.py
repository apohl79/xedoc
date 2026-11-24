"""Resolve and validate the pnpm version pinned by the workspace."""

import json
from pathlib import Path
import shutil
import subprocess

from xedoc_release_retry import run_with_retry


def pnpm_command(source_root: Path) -> list[str]:
    package_json = source_root / "package.json"
    package_manager = json.loads(package_json.read_text(encoding="utf-8")).get(
        "packageManager", ""
    )
    package_name, separator, package_version = package_manager.partition("@")
    if package_name != "pnpm" or not separator or not package_version:
        raise RuntimeError(
            f"Invalid packageManager in {package_json}: {package_manager!r}"
        )
    expected_version = package_version.partition("+")[0]

    corepack = shutil.which("corepack")
    if corepack is not None:
        command = [corepack, "pnpm"]
    else:
        pnpm = shutil.which("pnpm")
        if pnpm is None:
            raise RuntimeError(
                "Release preflight requires executable: pnpm or corepack"
            )
        command = [pnpm]

    try:
        result = run_with_retry(
            [*command, "--version"],
            cwd=source_root,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        detail = (
            f" {error.stderr.strip()}"
            if isinstance(error, subprocess.CalledProcessError) and error.stderr
            else ""
        )
        raise RuntimeError(
            f"The configured pnpm executable is unusable.{detail}"
        ) from error
    actual_version = result.stdout.strip()
    if actual_version != expected_version:
        raise RuntimeError(
            f"Release requires pnpm {expected_version}, but the selected "
            f"package manager reported {actual_version or 'no version'}."
        )
    return command
