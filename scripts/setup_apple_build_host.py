#!/usr/bin/env python3
"""Check a release Mac and optionally configure its host-only APNs credentials."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile


def run(command, *, data=None):
    result = subprocess.run(command, input=data, capture_output=True, timeout=20)
    if result.returncode:
        raise RuntimeError(f"{Path(command[0]).name} {command[1]} failed")
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check", action="store_true", help="Read-only; never create configuration."
    )
    parser.add_argument(
        "--ios-only", action="store_true", help="Skip desktop release prerequisites."
    )
    parser.add_argument(
        "--configure-apns",
        action="store_true",
        help="Validate Keychain keys and create host-gui/apns.json.",
    )
    parser.add_argument(
        "--xedoc-home",
        type=Path,
        default=Path(os.environ.get("XEDOC_HOME", "~/.xedoc")).expanduser(),
    )
    parser.add_argument("--team-id", default="62QZB3NCZR")
    parser.add_argument("--topic", default="org.e47.xedoc.mobile")
    parser.add_argument("--production-key-id", default="8277F92N2F")
    parser.add_argument("--sandbox-key-id", default="G68J93J939")
    parser.add_argument(
        "--notary-profile",
        help="Also validate an existing notarytool Keychain profile with Apple.",
    )
    args = parser.parse_args()
    if sys.version_info < (3, 10):
        raise RuntimeError(
            "Python 3.10+ must be on PATH for release helpers; "
            f"this interpreter is {sys.version.split()[0]}. "
            'Run brew install python@3.12, then export PATH="$(brew --prefix python@3.12)/libexec/bin:$PATH" '
            "in the release terminal and rerun this check."
        )
    if sys.platform != "darwin":
        raise RuntimeError("This setup requires macOS.")
    for value in (args.team_id, args.production_key_id, args.sandbox_key_id):
        if not re.fullmatch(r"[A-Z0-9]{10}", value):
            raise RuntimeError("Team and key IDs must be 10 uppercase letters/digits.")
    if not re.fullmatch(r"[A-Za-z0-9.-]{1,200}", args.topic):
        raise RuntimeError("Invalid APNs topic.")

    commands = ["xcodegen", "xcodebuild", "codesign", "ditto", "security", "openssl"]
    if not args.ios_only:
        commands += ["bazel", "uv", "gh", "git"]
    missing = [command for command in commands if shutil.which(command) is None]
    if missing:
        raise RuntimeError(
            f"Missing tools: {', '.join(missing)}. Follow ios/README.md build-host setup."
        )
    developer = run(["xcode-select", "-p"]).decode().strip()
    if not Path(developer, "Platforms/iPhoneOS.platform").is_dir():
        raise RuntimeError(
            "Select full Xcode with sudo xcode-select -s /Applications/Xcode.app/Contents/Developer."
        )
    sdk = run(["xcrun", "--sdk", "iphoneos", "--show-sdk-version"]).decode().strip()
    print(f"Xcode iOS SDK: {sdk}")
    identities = run(["security", "find-identity", "-v", "-p", "codesigning"]).decode()
    required = ["Apple Development:"]
    if not args.ios_only:
        required += ["Developer ID Application:"]
    for prefix in required:
        names = re.findall(r'"([^"]+)"', identities)
        matches = []
        for name in names:
            if name.startswith(prefix):
                certificate = run(["security", "find-certificate", "-c", name, "-p"])
                subject = run(
                    ["openssl", "x509", "-noout", "-subject", "-nameopt", "RFC2253"],
                    data=certificate,
                ).decode()
                if re.search(rf"(?:^|,)OU={args.team_id}(?:,|$)", subject):
                    matches.append(name)
        if not matches:
            raise RuntimeError(
                f"No valid {prefix} identity for team {args.team_id}. Use Xcode Settings > Accounts > Manage Certificates."
            )
        print(f"Signing identity: {matches[0]}")
    if args.notary_profile:
        run(
            [
                "xcrun",
                "notarytool",
                "history",
                "--keychain-profile",
                args.notary_profile,
            ]
        )
        print("Notarization profile: authenticated")
    elif not args.ios_only:
        print(
            "Notarization credentials not checked; use --notary-profile NAME to verify."
        )

    if not args.configure_apns:
        print(
            "Build-host checks passed. APNs sending credentials are not needed on a build-only Mac."
        )
        return
    config = {
        "teamId": args.team_id,
        "topic": args.topic,
        "keys": {
            environment: {"keyId": key_id, "keychainService": "org.e47.xedoc.apns"}
            for environment, key_id in (
                ("production", args.production_key_id),
                ("development", args.sandbox_key_id),
            )
        },
    }
    for environment, key in config["keys"].items():
        try:
            material = run(
                [
                    "security",
                    "find-generic-password",
                    "-s",
                    key["keychainService"],
                    "-a",
                    key["keyId"],
                    "-w",
                ]
            ).strip()
            if not material.startswith(b"-----BEGIN"):
                material = bytes.fromhex(material.decode("ascii"))
            if len(material) > 16 * 1024:
                raise ValueError("oversize key")
            public = run(["openssl", "pkey", "-pubout"], data=material)
            details = run(["openssl", "pkey", "-pubin", "-text", "-noout"], data=public)
            if b"ASN1 OID: prime256v1" not in details:
                raise ValueError("wrong curve")
        except (RuntimeError, ValueError):
            raise RuntimeError(
                f"{environment} APNs key {key['keyId']} missing or not P-256 in Keychain. "
                "Import from 1Password in one authorized persistent terminal; see ios/README.md."
            ) from None
        print(f"APNs {environment}: Keychain key {key['keyId']} is P-256")
    path = args.xedoc_home / "host-gui" / "apns.json"
    if path.is_symlink() or path.parent.is_symlink():
        raise RuntimeError(f"Refusing symlink configuration: {path}")
    if path.exists():
        if path.stat().st_size > 16 * 1024 or json.loads(path.read_text()) != config:
            raise RuntimeError(f"Conflicting configuration; left unchanged: {path}")
        if path.stat().st_uid != os.getuid() or path.stat().st_mode & 0o077:
            raise RuntimeError(
                f"Configuration must be owned by you and mode 0600: {path}"
            )
        print(f"APNs configuration already matches; unchanged: {path}")
    elif args.check:
        raise RuntimeError(
            f"APNs configuration missing: {path}. Run without --check to create it."
        )
    else:
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        with tempfile.NamedTemporaryFile(
            dir=path.parent, prefix=".apns-", mode="w"
        ) as stream:
            json.dump(config, stream, indent=2)
            stream.write("\n")
            stream.flush()
            os.link(stream.name, path)
        print(f"Created private APNs configuration: {path}")
    print(
        "APNs host setup passed. Key IDs/team/topic must match their Apple Developer portal records."
    )


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as error:
        print(f"Error: {error}", file=sys.stderr)
        raise SystemExit(1)
