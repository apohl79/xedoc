"""Build the Electron host app for Linux and Windows."""

from pathlib import Path
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import tempfile
from xedoc_release_retry import run_with_retry


# (electron-builder platform flag, target, electron-builder arch, release arch, extension)
DESKTOP_BUILDS = (
    ("--linux", "AppImage", "x64", "linux-x86_64", "AppImage"),
    ("--linux", "AppImage", "arm64", "linux-aarch64", "AppImage"),
    ("--linux", "deb", "x64", "linux-x86_64", "deb"),
    ("--linux", "deb", "arm64", "linux-aarch64", "deb"),
    ("--win", "nsis", "x64", "windows-x86_64-setup", "exe"),
)


def desktop_asset_name(version: str, platform_arch: str, extension: str) -> str:
    return f"xedoc-desktop-{version}-{platform_arch}.{extension}"


def build_desktop_release(
    *,
    source_root: Path,
    output_dir: Path,
    version: str,
    force: bool,
    pnpm_command: list[str],
    resume: bool = False,
) -> list[Path]:
    if not pnpm_command:
        raise RuntimeError("Building the Linux and Windows apps requires pnpm.")
    outputs = [
        output_dir / version / desktop_asset_name(version, platform_arch, extension)
        for _, _, _, platform_arch, extension in DESKTOP_BUILDS
    ]
    existing = [output for output in outputs if output.exists()]
    if existing and not force and not resume:
        raise RuntimeError(f"Archive output already exists: {existing[0]}")
    numeric_version = version.split("-", maxsplit=1)[0]
    outputs[0].parent.mkdir(parents=True, exist_ok=True)
    for (platform, target, arch, platform_arch, extension), output in zip(
        DESKTOP_BUILDS, outputs
    ):
        completion = output.with_name(f"{output.name}.complete.json")
        if resume and output.is_file() and completion.is_file():
            try:
                record = json.loads(completion.read_text(encoding="utf-8"))
                from xedoc_release_resume import validate_desktop_archive

                validate_desktop_archive(
                    output,
                    version=version,
                    platform_arch=platform_arch,
                    extension=extension,
                )
                if record.get("version") == version and record.get(
                    "sha256"
                ) == sha256_file(output):
                    print(
                        f"Resuming with completed desktop artifact: {output}",
                        flush=True,
                    )
                    continue
            except (OSError, ValueError, json.JSONDecodeError):
                pass
        completion.unlink(missing_ok=True)
        with tempfile.TemporaryDirectory(prefix="xedoc-desktop-release-") as temp_dir:
            run(
                [
                    *pnpm_command,
                    "--filter",
                    "xedoc-desktop",
                    "exec",
                    "electron-builder",
                    platform,
                    target,
                    f"--{arch}",
                    "--publish",
                    "never",
                    f"-c.extraMetadata.version={numeric_version}",
                    f"-c.directories.output={temp_dir}",
                ],
                cwd=source_root,
            )
            built = sorted(Path(temp_dir).glob(f"*.{extension}"))
            if len(built) != 1:
                raise RuntimeError(
                    f"Expected one {target} {arch} artifact, found: {built}"
                )
            shutil.move(str(built[0]), output)
        from xedoc_release_resume import validate_desktop_archive

        validate_desktop_archive(
            output,
            version=version,
            platform_arch=platform_arch,
            extension=extension,
        )
        temporary_completion = completion.with_suffix(f"{completion.suffix}.tmp")
        temporary_completion.write_text(
            json.dumps({"version": version, "sha256": sha256_file(output)}),
            encoding="utf-8",
        )
        os.replace(temporary_completion, completion)
    return outputs


def run(command: list[str], *, cwd: Path) -> None:
    print("+ " + shlex.join(command), flush=True)
    try:
        run_with_retry(command, cwd=cwd)
    except (OSError, subprocess.CalledProcessError) as err:
        raise RuntimeError(
            f"Desktop app release command failed: {shlex.join(command)}"
        ) from err


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()
