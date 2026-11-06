"""Default host credentials and authenticated, on-demand APNs provisioning."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid


KEYCHAIN_SERVICE = "org.e47.xedoc.apns"
KEYS = {
    "production": ("8277F92N2F", "Xedoc APNs Production"),
    "development": ("G68J93J939", "Xedoc APNs Sandbox"),
}


def default_configuration(home: Path) -> dict:
    return {
        "teamId": "62QZB3NCZR",
        "topic": "org.e47.xedoc.mobile",
        "keys": {
            environment: {
                "keyId": key_id,
                **(
                    {"keychainService": KEYCHAIN_SERVICE}
                    if sys.platform == "darwin"
                    else {
                        "privateKeyFile": str(
                            (home / "host-gui" / f"apns-{key_id}.p8").absolute()
                        )
                    }
                ),
            }
            for environment, (key_id, _) in KEYS.items()
        },
    }


def credential_exists(key: dict) -> bool:
    if sys.platform == "darwin":
        result = subprocess.run(
            [
                "/usr/bin/security",
                "find-generic-password",
                "-s",
                key["keychainService"],
                "-a",
                key["keyId"],
            ],
            capture_output=True,
            timeout=3,
        )
        return result.returncode == 0
    path = Path(key["privateKeyFile"])
    return path.exists() or path.is_symlink()


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("check", "setup"))
    parser.add_argument(
        "--xedoc-home",
        type=Path,
        default=Path(os.environ.get("XEDOC_HOME", "~/.xedoc")).expanduser(),
    )
    args = parser.parse_args(argv)
    from .host_apns import HostAPNs

    sender = HostAPNs(args.xedoc_home)
    try:
        custom = sender.path.exists() or sender.path.is_symlink()
        defaults = default_configuration(args.xedoc_home)
        config = sender.configuration() if custom else defaults
        missing = []
        for environment, key in config["keys"].items():
            if credential_exists(key):
                sender._token(config, environment)
            else:
                missing.append(environment)
        if missing and config != defaults:
            raise RuntimeError(
                "APNs credentials are unavailable; existing settings were not changed."
            )
        if missing and args.action == "check":
            print("APNs signing credentials are missing.", file=sys.stderr)
            return 2
        materials = []
        if missing:
            if not os.environ.get("OP_SESSION"):
                raise RuntimeError(
                    'Authenticate in this terminal with `eval "$(op signin)"` before provisioning.'
                )
            if not shutil.which("op"):
                raise RuntimeError(
                    "APNs provisioning requires the existing 1Password CLI."
                )
            separator = f"\n--{uuid.uuid4().hex}--\n"
            template = separator.join(
                "{{op://Private/" + KEYS[environment][1] + "/credential}}"
                for environment in missing
            )
            result = subprocess.run(
                ["op", "inject"],
                input=template.encode(),
                capture_output=True,
                timeout=180,
            )
            if (
                result.returncode
                or not 0 < len(result.stdout) <= len(missing) * 17 * 1024
            ):
                raise RuntimeError(
                    "Could not retrieve the missing APNs keys from 1Password."
                )
            materials = result.stdout.split(separator.encode())
            if len(materials) != len(missing):
                raise RuntimeError(
                    "1Password returned an invalid APNs credential response."
                )
        for environment, material in zip(missing, materials):
            material = material.strip()
            if not 0 < len(material) <= 16 * 1024:
                raise RuntimeError("APNs signing key exceeds the size limit.")
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric import ec

            private_key = serialization.load_pem_private_key(material, password=None)
            if not isinstance(
                private_key, ec.EllipticCurvePrivateKey
            ) or not isinstance(private_key.curve, ec.SECP256R1):
                raise RuntimeError("The APNs signing key must use P-256.")
            key = config["keys"][environment]
            if sys.platform == "darwin":
                # Interactive security input keeps the key out of process arguments.
                command = (
                    f"add-generic-password -s {KEYCHAIN_SERVICE} -a {key['keyId']} "
                    f"-T /usr/bin/security -w {material.hex()}\n"
                )
                imported = subprocess.run(
                    ["/usr/bin/security", "-i"],
                    input=command.encode(),
                    capture_output=True,
                    timeout=15,
                )
                if imported.returncode:
                    raise RuntimeError("Could not import the APNs key into Keychain.")
            else:
                path = Path(key["privateKeyFile"])
                if path.parent.is_symlink():
                    raise RuntimeError("Refusing a symlink APNs credential directory.")
                path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                with tempfile.NamedTemporaryFile(dir=path.parent) as stream:
                    stream.write(material)
                    stream.flush()
                    os.link(stream.name, path)
            sender._token(config, environment)
        print(json.dumps({"available": True, "environments": sorted(config["keys"])}))
        return 0
    except subprocess.TimeoutExpired:
        print(
            "APNs setup timed out waiting for credential access or approval.",
            file=sys.stderr,
        )
        return 1
    except (OSError, RuntimeError, ValueError, subprocess.SubprocessError) as error:
        # Never include subprocess output or key parsing details.
        message = (
            str(error) if isinstance(error, RuntimeError) else type(error).__name__
        )
        print(f"APNs setup failed: {message}", file=sys.stderr)
        return 1
    finally:
        sender.close()
