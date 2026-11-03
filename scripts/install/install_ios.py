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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path, help="Signed Xedoc.app directory")
    args = parser.parse_args()
    subprocess.run(
        ["codesign", "--verify", "--deep", "--strict", str(args.app)], check=True
    )
    with (args.app / "Info.plist").open("rb") as source:
        info = plistlib.load(source)
    if info.get("CFBundleIdentifier") != "org.e47.xedoc.mobile":
        parser.error("The archive does not contain the Xedoc iOS app.")
    profile = plistlib.loads(
        subprocess.check_output(
            ["security", "cms", "-D", "-i", str(args.app / "embedded.mobileprovision")],
            stderr=subprocess.DEVNULL,
        )
    )
    if profile["ExpirationDate"].replace(tzinfo=timezone.utc) <= datetime.now(
        timezone.utc
    ):
        parser.error("The app's signing profile expired. Download a newer release.")
    with open("/dev/tty") as terminal_input, open("/dev/tty", "w") as terminal_output:

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
                        current = next(
                            (
                                device
                                for device in wired_devices()
                                if device["identifier"] == identifier
                            ),
                            None,
                        )
                        if current is None:
                            print(
                                "The selected device is no longer connected by cable. "
                                "No installation attempted."
                            )
                        elif not profile.get("ProvisionsAllDevices") and (
                            current["hardwareProperties"]["udid"]
                            not in profile.get("ProvisionedDevices", [])
                        ):
                            print(
                                "This device is not included in the app's signing profile. "
                                "Register it with the signing team and rebuild the release."
                            )
                        else:
                            subprocess.run(
                                [
                                    "xcrun",
                                    "devicectl",
                                    "device",
                                    "install",
                                    "app",
                                    "--device",
                                    identifier,
                                    str(args.app),
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
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (EOFError, KeyboardInterrupt):
        print("\nDevice installation cancelled.")
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"iOS installation failed: {error}", file=sys.stderr)
        raise SystemExit(1)
