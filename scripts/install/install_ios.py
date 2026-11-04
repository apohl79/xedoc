#!/usr/bin/env python3
"""Interactively install a signed Xedoc app on cable-connected Apple devices."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile


def device_command(*arguments):
    with tempfile.TemporaryDirectory(prefix="xedoc-devices-") as directory:
        output = Path(directory) / "result.json"
        subprocess.run(
            ["xcrun", "devicectl", *arguments, "--json-output", str(output)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        return json.loads(output.read_text())["result"]


def wired_devices():
    devices = device_command("list", "devices")["devices"]
    return sorted(
        (
            device
            for device in devices
            if device.get("connectionProperties", {}).get("transportType") == "wired"
            and device["connectionProperties"].get("pairingState") == "paired"
            and device.get("hardwareProperties", {}).get("deviceType")
            in ("iPhone", "iPad")
            and device["hardwareProperties"].get("reality", "physical") == "physical"
        ),
        key=lambda device: device["deviceProperties"]["name"].casefold(),
    )


def read_signed_app(app):
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    with (app / "Info.plist").open("rb") as source:
        info = plistlib.load(source)
    if info.get("CFBundleIdentifier") != "org.e47.xedoc.mobile":
        raise ValueError("The archive does not contain the Xedoc iOS app.")
    profile = plistlib.loads(
        subprocess.check_output(
            ["security", "cms", "-D", "-i", str(app / "embedded.mobileprovision")],
            stderr=subprocess.DEVNULL,
        )
    )
    if profile["ExpirationDate"].replace(tzinfo=timezone.utc) <= datetime.now(
        timezone.utc
    ):
        raise ValueError("The app's signing profile expired. Download a newer release.")
    return info, profile


def provision_app(app, info, profile, udid, directory):
    project = app.parent / "ios-project/Xedoc.xcodeproj"
    if not project.is_dir():
        raise ValueError(
            "This release has no automatic-provisioning project. "
            "Download an updated iOS package."
        )
    teams = profile.get("TeamIdentifier", [])
    if len(teams) != 1:
        raise ValueError("The app's signing team could not be determined.")
    team = teams[0]
    build_root = Path(tempfile.mkdtemp(prefix="sign-", dir=directory))
    archive = build_root / "Xedoc.xcarchive"
    options = build_root / "ExportOptions.plist"
    with options.open("wb") as source:
        plistlib.dump(
            {
                "method": "release-testing",
                "signingStyle": "automatic",
                "teamID": team,
                "thinning": "<none>",
            },
            source,
        )
    print(
        f"Registering device {udid} with signing team {team} and building "
        "a signed app using your Xcode account…",
        flush=True,
    )
    signing_command = [
        "xcodebuild",
        "-project",
        str(project),
        "-scheme",
        "Xedoc",
        "-configuration",
        "Release",
        "-derivedDataPath",
        str(build_root / "derived"),
        "-allowProvisioningUpdates",
        "-allowProvisioningDeviceRegistration",
        "CODE_SIGN_STYLE=Automatic",
        "CODE_SIGN_IDENTITY=Apple Development",
        f"DEVELOPMENT_TEAM={team}",
        f"MARKETING_VERSION={info['CFBundleShortVersionString']}",
        f"CURRENT_PROJECT_VERSION={info['CFBundleVersion']}",
    ]
    try:
        # Archiving uses a generic destination and cannot register a new device.
        subprocess.run(
            [*signing_command, "-destination", f"platform=iOS,id={udid}", "build"],
            check=True,
        )
        subprocess.run(
            [
                *signing_command,
                "-destination",
                "generic/platform=iOS",
                "-archivePath",
                str(archive),
                "archive",
            ],
            check=True,
        )
        export = build_root / "export"
        subprocess.run(
            [
                "xcodebuild",
                "-exportArchive",
                "-archivePath",
                str(archive),
                "-exportPath",
                str(export),
                "-exportOptionsPlist",
                str(options),
                "-allowProvisioningUpdates",
            ],
            check=True,
        )
    except subprocess.CalledProcessError as error:
        raise ValueError(
            "Automatic device provisioning failed. Sign in to the app's signing team "
            f"({team}) in Xcode Settings → Accounts and retry. "
            "The account must allow device registration and distribution signing; "
            "Apple device limits or processing delays can also block installation. "
            "See the Xcode error above."
        ) from error
    packages = list(export.glob("*.ipa"))
    if len(packages) != 1:
        raise ValueError("Expected exactly one exported Ad Hoc iOS app.")
    unpacked = build_root / "unpacked"
    subprocess.run(["ditto", "-x", "-k", str(packages[0]), str(unpacked)], check=True)
    signed_app = unpacked / "Payload/Xedoc.app"
    signed_info, signed_profile = read_signed_app(signed_app)
    if (
        signed_profile.get("TeamIdentifier") != teams
        or signed_profile.get("Entitlements", {}).get("get-task-allow", True)
        or signed_profile.get("ProvisionsAllDevices")
        or udid not in signed_profile.get("ProvisionedDevices", [])
        or any(
            signed_info.get(key) != info[key]
            for key in ("CFBundleShortVersionString", "CFBundleVersion")
        )
    ):
        raise ValueError(
            "Xcode did not export the requested version with an Ad Hoc profile "
            "covering this device and signing team. No installation attempted."
        )
    return signed_app, signed_info, signed_profile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="Signed Xedoc.app directory")
    args = parser.parse_args()
    app = args.app.resolve()
    info, profile = read_signed_app(app)
    failed = False
    with (
        tempfile.TemporaryDirectory(prefix="xedoc-ios-install-") as directory,
        open("/dev/tty") as terminal_input,
        open("/dev/tty", "w") as terminal_output,
    ):

        def ask(message):
            terminal_output.write(message)
            terminal_output.flush()
            answer = terminal_input.readline()
            if not answer:
                raise EOFError
            return answer.strip()

        while True:
            ask(
                "Connect and unlock an iPhone or iPad by cable, trust this Mac, "
                "then press Return to list devices: "
            )
            try:
                devices = wired_devices()
                if not devices:
                    print(
                        "No paired, cable-connected iPhone or iPad found. "
                        "Check the cable and trust prompt; enable Developer Mode."
                    )
                else:
                    for number, device in enumerate(devices, 1):
                        properties = device["deviceProperties"]
                        hardware = device["hardwareProperties"]
                        print(
                            f"{number}. {properties['name']} — "
                            f"{hardware.get('marketingName', hardware['deviceType'])} "
                            f"({hardware['udid']})",
                            flush=True,
                        )
                    selected = None
                    while selected is None:
                        choice = ask("Choose a device number, or q to cancel: ")
                        if choice.lower() == "q":
                            break
                        if (
                            choice.isascii()
                            and choice.isdigit()
                            and 1 <= int(choice) <= len(devices)
                        ):
                            selected = devices[int(choice) - 1]
                        else:
                            print("Enter a listed device number or q.", flush=True)
                    if selected is not None:
                        identifier = selected["identifier"]
                        details = device_command(
                            "device", "info", "details", "--device", identifier
                        )
                        if (
                            details["deviceProperties"].get("developerModeStatus")
                            == "disabled"
                        ):
                            print(
                                "Enable Developer Mode on the device in Settings → "
                                "Privacy & Security → Developer Mode. Allow the restart, "
                                "then unlock and confirm Turn On. Keep the cable connected.",
                                flush=True,
                            )
                            ask("Press Return after enabling Developer Mode: ")
                            details = device_command(
                                "device", "info", "details", "--device", identifier
                            )
                            if (
                                details["deviceProperties"].get("developerModeStatus")
                                != "enabled"
                            ):
                                raise ValueError(
                                    "Developer Mode is not enabled yet. Enable it on "
                                    "the device, reconnect by cable, and retry."
                                )
                        current = next(
                            (
                                device
                                for device in wired_devices()
                                if device["identifier"] == identifier
                            ),
                            None,
                        )
                        if current is None:
                            raise ValueError(
                                "The selected device is no longer connected by cable. "
                                "No installation attempted."
                            )
                        if not profile.get("ProvisionsAllDevices") and (
                            current["hardwareProperties"]["udid"]
                            not in profile.get("ProvisionedDevices", [])
                        ):
                            app, info, profile = provision_app(
                                args.app.resolve(),
                                info,
                                profile,
                                current["hardwareProperties"]["udid"],
                                directory,
                            )
                        if not any(
                            device["identifier"] == identifier
                            for device in wired_devices()
                        ):
                            raise ValueError(
                                "The selected device disconnected during provisioning. "
                                "Reconnect it by cable and retry."
                            )
                        subprocess.run(
                            [
                                "xcrun",
                                "devicectl",
                                "device",
                                "install",
                                "app",
                                "--device",
                                identifier,
                                str(app),
                            ],
                            check=True,
                        )
                        print(
                            f"Xedoc {info['CFBundleShortVersionString']} installed on "
                            f"{current['deviceProperties']['name']}.",
                            flush=True,
                        )
            except (
                subprocess.CalledProcessError,
                OSError,
                ValueError,
                KeyError,
            ) as error:
                failed = True
                print(
                    f"Device installation failed: {error}. "
                    "Check the cable, pairing, Developer Mode, and signing profile.",
                    file=sys.stderr,
                )
            if ask("Install on another iPhone or iPad? [y/N] ").lower() not in (
                "y",
                "yes",
            ):
                break
    return 1 if failed else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (EOFError, KeyboardInterrupt):
        print("\nDevice installation cancelled.")
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"iOS installation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
