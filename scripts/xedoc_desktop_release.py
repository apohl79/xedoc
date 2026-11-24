"""Build the Electron host app for Linux and Windows."""

from pathlib import Path
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
) -> list[Path]:
    if shutil.which("pnpm") is None:
        raise RuntimeError("Building the Linux and Windows apps requires pnpm.")
    outputs = [
        output_dir / version / desktop_asset_name(version, platform_arch, extension)
        for _, _, _, platform_arch, extension in DESKTOP_BUILDS
    ]
    existing = [output for output in outputs if output.exists()]
    if existing and not force:
        raise RuntimeError(f"Archive output already exists: {existing[0]}")
    numeric_version = version.split("-", maxsplit=1)[0]
    run(["pnpm", "install", "--frozen-lockfile"], cwd=source_root)
    outputs[0].parent.mkdir(parents=True, exist_ok=True)
    for (platform, target, arch, _, extension), output in zip(DESKTOP_BUILDS, outputs):
        with tempfile.TemporaryDirectory(prefix="xedoc-desktop-release-") as temp_dir:
            run(
                [
                    "pnpm",
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
    return outputs


def run(command: list[str], *, cwd: Path) -> None:
    print("+ " + shlex.join(command), flush=True)
    try:
        run_with_retry(command, cwd=cwd)
    except (OSError, subprocess.CalledProcessError) as err:
        raise RuntimeError(
            f"Desktop app release command failed: {shlex.join(command)}"
        ) from err
