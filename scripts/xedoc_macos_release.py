"""Build the universal Developer ID-signed Electron host app."""

from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import tempfile


def build_macos_release(
    *,
    source_root: Path,
    output_dir: Path,
    version: str,
    force: bool,
    codesign_identity: str,
    notarize: bool,
    notarytool_keychain_profile: str | None = None,
) -> Path:
    if sys.platform != "darwin":
        raise RuntimeError("Building the macOS app requires macOS and Xcode tools.")
    for command in ("pnpm", "codesign", "ditto", "lipo", "xcrun"):
        if shutil.which(command) is None:
            raise RuntimeError(f"Building the macOS app requires {command}.")
    archive_output = output_dir / version / f"xedoc-macos-{version}.zip"
    if archive_output.exists() and not force:
        raise RuntimeError(f"Archive output already exists: {archive_output}")
    numeric_version = version.split("-", maxsplit=1)[0]
    signing_name = codesign_identity.removeprefix("Developer ID Application: ")
    run(["pnpm", "install", "--frozen-lockfile"], cwd=source_root)
    archive_output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="xedoc-macos-release-") as temp_dir:
        # electron-builder signs nested Chromium frameworks and helper apps inside-out.
        run(
            [
                "pnpm",
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
        subprocess.run(command, check=True, cwd=cwd)
    except (OSError, subprocess.CalledProcessError) as err:
        raise RuntimeError(
            f"macOS release command failed: {shlex.join(command)}"
        ) from err
