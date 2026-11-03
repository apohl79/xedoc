"""Build a development-signed iPhone/iPad release for cable installation."""

from pathlib import Path
import plistlib
import shlex
import shutil
import subprocess
import sys
import tempfile


def build_ios_release(
    *, source_root: Path, output_dir: Path, version: str, force: bool
) -> Path:
    if sys.platform != "darwin":
        raise RuntimeError("Building the iOS app requires macOS and Xcode.")
    for command in ("xcodegen", "xcodebuild", "codesign", "ditto"):
        if shutil.which(command) is None:
            raise RuntimeError(f"Building the iOS app requires {command}.")

    installer = source_root / "scripts/install/install_ios.py"
    if not installer.is_file():
        raise RuntimeError(f"Missing iOS device installer: {installer}")
    archive_output = output_dir / version / f"xedoc-ios-{version}.zip"
    if archive_output.exists() and not force:
        raise RuntimeError(f"Archive output already exists: {archive_output}")

    ios_root = source_root / "ios"
    derived_data = output_dir / ".ios-derived-data"
    numeric_version = version.split("-", maxsplit=1)[0]
    run(["xcodegen", "generate", "--spec", str(ios_root / "project.yml")])
    run(
        [
            "xcodebuild",
            "-project",
            str(ios_root / "Xedoc.xcodeproj"),
            "-scheme",
            "Xedoc",
            "-configuration",
            "Release",
            "-destination",
            "generic/platform=iOS",
            "-derivedDataPath",
            str(derived_data),
            "-allowProvisioningUpdates",
            "CODE_SIGN_IDENTITY=Apple Development",
            f"MARKETING_VERSION={numeric_version}",
            f"CURRENT_PROJECT_VERSION={numeric_version}",
            "build",
        ]
    )
    app = derived_data / "Build/Products/Release-iphoneos/Xedoc.app"
    if not (app / "embedded.mobileprovision").is_file():
        raise RuntimeError(f"Built iOS app has no device provisioning profile: {app}")
    with (app / "Info.plist").open("rb") as info_file:
        info = plistlib.load(info_file)
    if info.get("CFBundleShortVersionString") != numeric_version:
        raise RuntimeError("The built iOS app version does not match the release.")
    run(["codesign", "--verify", "--deep", "--strict", str(app)])

    archive_output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="xedoc-ios-release-") as temp_dir:
        staging = Path(temp_dir) / "package"
        staging.mkdir()
        run(["ditto", "--norsrc", "--noextattr", str(app), str(staging / app.name)])
        shutil.copy2(installer, staging / "install-ios.py")
        temporary_archive = Path(temp_dir) / archive_output.name
        run(
            [
                "ditto",
                "-c",
                "-k",
                "--norsrc",
                "--noextattr",
                str(staging),
                str(temporary_archive),
            ]
        )
        shutil.move(str(temporary_archive), archive_output)
    return archive_output


def run(command: list[str]) -> None:
    print("+ " + shlex.join(command), flush=True)
    try:
        subprocess.run(command, check=True)
    except (OSError, subprocess.CalledProcessError) as err:
        raise RuntimeError(
            f"iOS release command failed: {shlex.join(command)}"
        ) from err
