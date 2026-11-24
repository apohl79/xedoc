"""Build the universal Developer ID-signed Electron host app."""

from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import zipfile
from xedoc_release_retry import run_with_retry
from xedoc_release_resume import validate_app_archive


def build_macos_release(
    *,
    source_root: Path,
    output_dir: Path,
    version: str,
    force: bool,
    codesign_identity: str,
    notarize: bool,
    pnpm_command: list[str],
    resume: bool = False,
    notarytool_keychain_profile: str | None = None,
) -> Path:
    if sys.platform != "darwin":
        raise RuntimeError("Building the macOS app requires macOS and Xcode tools.")
    for command in (pnpm_command[0], "codesign", "ditto", "lipo", "xcrun"):
        if shutil.which(command) is None:
            raise RuntimeError(f"Building the macOS app requires {command}.")
    archive_output = output_dir / version / f"xedoc-macos-{version}.zip"
    if resume and archive_output.is_file():
        try:
            validate_app_archive(
                archive_output,
                version=version.split("-", maxsplit=1)[0],
                info_plist_path="Xedoc.app/Contents/Info.plist",
            )
            notarization_summary = (
                output_dir
                / version
                / "notarization/macos-app/notarization.zip-notarization-summary.txt"
            )
            notarization_complete = notarization_summary.is_file() and (
                "notarization=completed"
                in notarization_summary.read_text(encoding="utf-8").splitlines()
            )
            if not notarize or notarization_complete:
                print(
                    f"Resuming with completed macOS archive: {archive_output}",
                    flush=True,
                )
                return archive_output
        except (KeyError, OSError, ValueError, zipfile.BadZipFile):
            pass
        archive_output.unlink()
    if archive_output.exists() and not force and not resume:
        raise RuntimeError(f"Archive output already exists: {archive_output}")
    numeric_version = version.split("-", maxsplit=1)[0]
    signing_name = codesign_identity.removeprefix("Developer ID Application: ")
    archive_output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="xedoc-macos-release-") as temp_dir:
        # electron-builder signs nested Chromium frameworks and helper apps inside-out.
        run(
            [
                *pnpm_command,
                "--filter",
                "xedoc-desktop",
                "exec",
                "electron-builder",
                "--mac",
                "--universal",
                "--dir",
                "--publish",
                "never",
                f"-c.extraMetadata.version={numeric_version}",
                f"-c.mac.identity={signing_name}",
                "-c.forceCodeSigning=true",
                f"-c.directories.output={temp_dir}",
            ],
            cwd=source_root,
        )
        app = Path(temp_dir) / "mac-universal/Xedoc.app"
        with (app / "Contents/Info.plist").open("rb") as info_file:
            info = plistlib.load(info_file)
        if (
            info.get("CFBundleIdentifier") != "org.e47.xedoc.desktop"
            or info.get("CFBundleShortVersionString") != numeric_version
        ):
            raise RuntimeError(
                "The macOS app identity or version does not match the release."
            )
        run(
            [
                "lipo",
                str(app / "Contents/MacOS" / info["CFBundleExecutable"]),
                "-verify_arch",
                "arm64",
                "x86_64",
            ]
        )
        run(["codesign", "--verify", "--deep", "--strict", str(app)])
        temporary_archive = Path(temp_dir) / archive_output.name
        if notarize:
            submission = Path(temp_dir) / "notarization.zip"
            run(["ditto", "-c", "-k", "--keepParent", str(app), str(submission)])
            command = [
                str(
                    source_root
                    / ".github/scripts/macos-signing/notarize_macos_binary_with_rcodesign.sh"
                ),
                "--archive",
                str(submission),
                "--report-dir",
                str(output_dir / version / "notarization/macos-app"),
            ]
            if notarytool_keychain_profile:
                command.extend(
                    ["--notarytool-keychain-profile", notarytool_keychain_profile]
                )
            run(command)
            run(["xcrun", "stapler", "staple", str(app)])
            run(["xcrun", "stapler", "validate", str(app)])
            run(["codesign", "--verify", "--deep", "--strict", str(app)])
            run(["spctl", "--assess", "--type", "execute", "--verbose=2", str(app)])
        run(["ditto", "-c", "-k", "--keepParent", str(app), str(temporary_archive)])
        shutil.move(str(temporary_archive), archive_output)
    return archive_output


def run(command: list[str], *, cwd: Path | None = None) -> None:
    print("+ " + shlex.join(command), flush=True)
    try:
        if (
            command[:1] == ["pnpm"]
            or command[1:2] == ["pnpm"]
            or command[1:3] == ["stapler", "staple"]
        ):
            run_with_retry(command, cwd=cwd)
        else:
            subprocess.run(command, check=True, cwd=cwd)
    except (OSError, subprocess.CalledProcessError) as err:
        raise RuntimeError(
            f"macOS release command failed: {shlex.join(command)}"
        ) from err
