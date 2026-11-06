"""Host-only APNs credentials and pooled HTTP/2 notification delivery."""

import base64
import json
import logging
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
import uuid

import httpcore
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, utils

from .apns_credentials import credential_exists, default_configuration


class HostAPNs:
    def __init__(self, home: Path) -> None:
        self.path = home / "host-gui" / "apns.json"
        self._tokens = {}
        self._pool = None

    def configuration(self) -> dict:
        if not self.path.exists() and not self.path.is_symlink():
            value = default_configuration(self.path.parent.parent)
            value["keys"] = {
                environment: key
                for environment, key in value["keys"].items()
                if credential_exists(key)
            }
            if not value["keys"]:
                raise OSError("APNs signing credentials are unavailable")
        else:
            if self.path.stat().st_size > 16 * 1024:
                raise ValueError("APNs configuration exceeds size limit")
            value = json.loads(self.path.read_text())
        if (
            not isinstance(value, dict)
            or not re.fullmatch(r"[A-Z0-9]{10}", str(value.get("teamId", "")))
            or not re.fullmatch(r"[A-Za-z0-9.-]{1,200}", str(value.get("topic", "")))
            or not isinstance(value.get("keys"), dict)
            or not value["keys"]
        ):
            raise ValueError("Invalid APNs configuration")
        for environment, key in value["keys"].items():
            if (
                environment not in ("development", "production")
                or not isinstance(key, dict)
                or not re.fullmatch(r"[A-Z0-9]{10}", str(key.get("keyId", "")))
            ):
                raise ValueError("Invalid APNs key configuration")
            source = key.get(
                "keychainService" if sys.platform == "darwin" else "privateKeyFile"
            )
            if not isinstance(source, str) or not 1 <= len(source) <= 1024:
                raise ValueError("APNs key source is missing")
            if sys.platform != "darwin" and not Path(source).is_absolute():
                raise ValueError("APNs private key path must be absolute")
        return value

    def _token(self, config: dict, environment: str) -> str:
        key = config["keys"][environment]
        identity = (config["teamId"], tuple(sorted(key.items())))
        cached = self._tokens.get(environment)
        now = int(time.time())
        if cached and cached[0] == identity and 0 <= now - cached[1] < 50 * 60:
            return cached[2]
        if sys.platform == "darwin":
            result = subprocess.run(
                [
                    "/usr/bin/security",
                    "find-generic-password",
                    "-s",
                    key["keychainService"],
                    "-a",
                    key["keyId"],
                    "-w",
                ],
                capture_output=True,
                timeout=3,
                check=False,
            )
            if result.returncode or len(result.stdout) > 16 * 1024:
                raise OSError("APNs signing key is unavailable in Keychain")
            material = result.stdout.strip()
            if not material.startswith(b"-----BEGIN"):
                material = bytes.fromhex(material.decode("ascii"))
        else:
            descriptor = os.open(key["privateKeyFile"], os.O_RDONLY | os.O_NOFOLLOW)
            with os.fdopen(descriptor, "rb") as stream:
                info = os.fstat(stream.fileno())
                if (
                    not stat.S_ISREG(info.st_mode)
                    or info.st_uid != os.getuid()
                    or info.st_mode & 0o077
                    or info.st_size > 16 * 1024
                ):
                    raise OSError("APNs signing key must be owner-private")
                material = stream.read(16 * 1024 + 1)
        private_key = serialization.load_pem_private_key(material, password=None)
        if not isinstance(private_key, ec.EllipticCurvePrivateKey) or not isinstance(
            private_key.curve, ec.SECP256R1  # gitleaks:allow -- P-256 curve check, not key material.
        ):
            raise ValueError("APNs requires a P-256 signing key")
        header = {"alg": "ES256", "kid": key["keyId"]}
        claims = {"iss": config["teamId"], "iat": now}
        encoded = [
            base64.urlsafe_b64encode(
                json.dumps(part, separators=(",", ":")).encode()
            ).rstrip(b"=")
            for part in (header, claims)
        ]
        unsigned = b".".join(encoded)
        signature = private_key.sign(unsigned, ec.ECDSA(hashes.SHA256()))
        first, second = utils.decode_dss_signature(signature)
        raw_signature = first.to_bytes(32, "big") + second.to_bytes(32, "big")
        token = (
            unsigned + b"." + base64.urlsafe_b64encode(raw_signature).rstrip(b"=")
        ).decode()
        self._tokens[environment] = (identity, now, token)
        return token

    def send(self, device: dict, payload: dict) -> str:
        config = self.configuration()
        environment = device["environment"]
        if environment not in config["keys"]:
            return "rejected"
        token = self._token(config, environment)
        if self._pool is None:
            self._pool = httpcore.ConnectionPool(
                http1=False,
                http2=True,
                max_connections=2,
                max_keepalive_connections=2,
                keepalive_expiry=300,
            )
        hostname = (
            "api.sandbox.push.apple.com"
            if environment == "development"
            else "api.push.apple.com"
        )
        notification_id = str(uuid.uuid4())
        content = json.dumps(payload, separators=(",", ":")).encode()
        if len(content) > 4096:
            raise ValueError("APNs payload exceeds size limit")
        headers = {
            "authorization": f"bearer {token}",
            "apns-topic": config["topic"],
            "apns-push-type": "alert",
            "apns-priority": "10",
            "apns-expiration": "0",
            "apns-id": notification_id,
            "content-type": "application/json",
        }
        with self._pool.stream(
            "POST",
            f"https://{hostname}/3/device/{device['deviceToken']}",
            headers=headers,
            content=content,
            extensions={
                "timeout": dict.fromkeys(("connect", "read", "write", "pool"), 2.0)
            },
        ) as response:
            body = bytearray()
            for chunk in response.iter_stream():
                body.extend(chunk)
                if len(body) > 2048:
                    raise OSError("APNs response exceeds size limit")
            status = response.status
        if status == 200:
            return "sent"
        try:
            reason = json.loads(body).get("reason", "Unknown")
        except (ValueError, AttributeError):
            reason = "Unknown"
        if not isinstance(reason, str) or not re.fullmatch(r"[A-Za-z]{1,64}", reason):
            reason = "Unknown"
        logging.warning(
            "APNs rejected notification",
            extra={
                "apnsStatus": status,
                "apnsReason": reason,
                "notificationId": notification_id,
            },
        )
        if status == 410 or reason in ("BadDeviceToken", "DeviceTokenNotForTopic"):
            return "invalid"
        if reason == "ExpiredProviderToken":
            self._tokens.pop(environment, None)
            return "retry"
        return "retry" if status in (429, 500, 503) else "rejected"

    def close(self) -> None:
        if self._pool is not None:
            self._pool.close()
            self._pool = None
