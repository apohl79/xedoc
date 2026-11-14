"""Build the universal Developer ID-signed macOS host app."""

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
        raise RuntimeError("Building the macOS app requires macOS and Xcode.")
    for command in ("xcodegen", "xcodebuild", "codesign", "ditto", "lipo", "xcrun"):
        if shutil.which(command) is None:
            raise RuntimeError(f"Building the macOS app requires {command}.")
    archive_output = output_dir / version / f"xedoc-macos-{version}.zip"
    if archive_output.exists() and not force:
        raise RuntimeError(f"Archive output already exists: {archive_output}")
    macos_root = source_root / "macos"
    derived_data = output_dir / ".macos-derived-data"
    numeric_version = version.split("-", maxsplit=1)[0]
    run(["xcodegen", "generate", "--spec", str(macos_root / "project.yml")])
    run(
        [
            "xcodebuild",
            "-project",
            str(macos_root / "Xedoc.xcodeproj"),
            "-scheme",
            "Xedoc",
            "-configuration",
            "Release",
            "-destination",
            "generic/platform=macOS",
            "-derivedDataPath",
            str(derived_data),
            "ARCHS=arm64 x86_64",
            "ONLY_ACTIVE_ARCH=NO",
            "CODE_SIGNING_ALLOWED=NO",
            f"MARKETING_VERSION={numeric_version}",
            f"CURRENT_PROJECT_VERSION={numeric_version}",
            "build",
        ]
    )
    archive_output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="xedoc-macos-release-") as temp_dir:
        app = Path(temp_dir) / "Xedoc.app"
        run(["ditto", str(derived_data / "Build/Products/Release/Xedoc.app"), str(app)])
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
        run(
            [
                "codesign",
                "--force",
                "--options",
                "runtime",
                "--timestamp",
                "--sign",
                codesign_identity,
                str(app),
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


def run(command: list[str]) -> None:
    print("+ " + shlex.join(command), flush=True)
    try:
        subprocess.run(command, check=True)
    except (OSError, subprocess.CalledProcessError) as err:
        raise RuntimeError(
            f"macOS release command failed: {shlex.join(command)}"
        ) from err
