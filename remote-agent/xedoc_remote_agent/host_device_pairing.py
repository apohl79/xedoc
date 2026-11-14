"""Owner-issued, single-use invitations for app-managed client identities."""

import argparse
import base64
from contextlib import contextmanager
from dataclasses import replace
from datetime import datetime, timedelta, timezone
import hashlib
import json
import os
from pathlib import Path
import secrets
import sqlite3
import ssl
import stat
import sys
import time

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

from .host_gui_config import HostGuiConfig, _write_file


class HostDevicePairing:
    def __init__(self, home: Path):
        self.directory = home / "host-gui" / "device-pairing"
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        info = self.directory.lstat()
        if (
            not stat.S_ISDIR(info.st_mode)
            or (os.name != "nt" and (info.st_uid != os.getuid() or info.st_mode & 0o077))
        ):
            raise ValueError("Device pairing directory must be private")
        self.ca_path = self.directory / "ca.pem"
        self.key_path = self.directory / "ca-key.pem"
        self.database = self.directory / "devices.sqlite"
        try:
            descriptor = os.open(self.database, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
            os.close(descriptor)
        except FileExistsError:
            pass
        info = self.database.lstat()
        if (
            not stat.S_ISREG(info.st_mode)
            or (os.name != "nt" and (info.st_uid != os.getuid() or info.st_mode & 0o077))
        ):
            raise ValueError("Device pairing database must be private")
        with self._connect() as connection:
            connection.executescript(
                "CREATE TABLE IF NOT EXISTS invite "
                "(id INTEGER PRIMARY KEY CHECK(id=1), digest TEXT, expires REAL);"
                "CREATE TABLE IF NOT EXISTS devices "
                "(fingerprint TEXT PRIMARY KEY, name TEXT, expires REAL);"
            )

    @contextmanager
    def _connect(self):
        connection = sqlite3.connect(self.database, timeout=10)
        try:
            with connection:
                yield connection
        finally:
            connection.close()

    def _authority(self):
        if not self.ca_path.exists() and not self.key_path.exists():
            key = ec.generate_private_key(ec.SECP256R1())
            name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Xedoc device CA")])
            now = datetime.now(timezone.utc)
            certificate = (
                x509.CertificateBuilder().subject_name(name).issuer_name(name)
                .public_key(key.public_key()).serial_number(x509.random_serial_number())
                .not_valid_before(now - timedelta(minutes=5))
                .not_valid_after(now + timedelta(days=3650))
                .add_extension(x509.BasicConstraints(ca=True, path_length=0), critical=True)
                .add_extension(x509.KeyUsage(
                    digital_signature=True, content_commitment=False,
                    key_encipherment=False, data_encipherment=False,
                    key_agreement=False, key_cert_sign=True, crl_sign=False,
                    encipher_only=None, decipher_only=None,
                ), critical=True)
                .sign(key, hashes.SHA256())
            )
            _write_file(self.key_path, key.private_bytes(
                serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            ))
            _write_file(self.ca_path, certificate.public_bytes(serialization.Encoding.PEM))
        key = serialization.load_pem_private_key(self.key_path.read_bytes(), password=None)
        certificate = x509.load_pem_x509_certificate(self.ca_path.read_bytes())
        if (
            not isinstance(key, ec.EllipticCurvePrivateKey)
            or key.public_key().public_bytes(
                serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
            ) != certificate.public_key().public_bytes(
                serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
            )
            or not certificate.extensions.get_extension_for_class(x509.BasicConstraints).value.ca
        ):
            raise ValueError("Invalid device CA")
        return key, certificate

    def invite(self, config: HostGuiConfig, server_ca: Path) -> str:
        return self.invite_details(config, server_ca)["invitation"]

    def invite_details(self, config: HostGuiConfig, server_ca: Path) -> dict:
        config.validate()
        if config.tls_cert is None or not config.origin.startswith("https://"):
            raise ValueError("Configure private-CA HTTPS before pairing devices")
        try:
            ca = x509.load_pem_x509_certificate(server_ca.read_bytes())
            leaf = x509.load_pem_x509_certificate(config.tls_cert.read_bytes())
            leaf.verify_directly_issued_by(ca)
            if not ca.extensions.get_extension_for_class(x509.BasicConstraints).value.ca:
                raise ValueError("Server CA is not a CA certificate")
        except (OSError, ValueError, InvalidSignature, x509.ExtensionNotFound) as error:
            raise ValueError(
                "Pairing requires the host's server CA at host-gui/ca-cert.pem; "
                "for a custom CA, create the invitation with device-pairing invite --server-ca"
            ) from error
        self._authority()
        token = secrets.token_urlsafe(32)
        digest = hashlib.sha256(token.encode()).hexdigest()
        expires = int(time.time()) + 600
        with self._connect() as connection:
            connection.execute(
                "INSERT OR REPLACE INTO invite VALUES (1, ?, ?)",
                (digest, expires),
            )
        value = {
            "version": 1, "origin": config.origin, "token": token,
            "serverCA": base64.b64encode(ca.public_bytes(serialization.Encoding.DER)).decode(),
            "serverCertificate": base64.b64encode(
                leaf.public_bytes(serialization.Encoding.DER)
            ).decode(),
        }
        invitation = base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip("=")
        return {"invitation": invitation, "invitationId": digest, "expiresAt": expires}

    def invitation_status(self, invitation_id: str) -> dict:
        if (
            not isinstance(invitation_id, str)
            or len(invitation_id) != 64
            or any(char not in "0123456789abcdef" for char in invitation_id)
        ):
            raise ValueError("Expected an invitation ID")
        with self._connect() as connection:
            row = connection.execute("SELECT digest, expires FROM invite WHERE id=1").fetchone()
        if row is None or not secrets.compare_digest(row[0], invitation_id):
            status = "replaced"
        elif row[1] == 0:
            status = "completed"
        elif row[1] <= time.time():
            status = "expired"
        else:
            status = "pending"
        return {"status": status}

    def enroll(self, body: dict, origin: str) -> dict:
        if set(body) != {"token", "publicKey", "signature", "name"}:
            raise ValueError("Invalid device enrollment")
        token, public, signature, name = (
            body[field] for field in ("token", "publicKey", "signature", "name")
        )
        if (
            not all(isinstance(value, str) for value in (token, public, signature, name))
            or len(token) != 43 or len(public) != 88 or len(signature) > 104
            or not 1 <= len(name) <= 64 or any(ord(char) < 32 for char in name)
        ):
            raise ValueError("Invalid device enrollment")
        try:
            raw = base64.b64decode(public, validate=True)
            if len(raw) != 65:
                raise ValueError("Invalid device public key")
            key = ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), raw)
            proof = f"xedoc-device-enroll-v1\n{origin}\n{token}\n{public}\n{name}"
            key.verify(
                base64.b64decode(signature, validate=True), proof.encode(),
                ec.ECDSA(hashes.SHA256()),
            )
        except InvalidSignature as error:
            raise PermissionError("Invalid device proof") from error
        now = datetime.now(timezone.utc)
        expires = now + timedelta(days=90)
        with self._connect() as connection:
            connection.execute("BEGIN IMMEDIATE")
            row = connection.execute("SELECT digest, expires FROM invite WHERE id=1").fetchone()
            if (
                row is None or row[1] <= time.time()
                or not secrets.compare_digest(row[0], hashlib.sha256(token.encode()).hexdigest())
            ):
                raise PermissionError("Invalid or expired device invitation")
            connection.execute("DELETE FROM devices WHERE expires<=?", (time.time(),))
            if connection.execute("SELECT COUNT(*) FROM devices").fetchone()[0] >= 128:
                raise ValueError("Device limit reached; revoke a device first")
            authority_key, authority = self._authority()
            if authority.not_valid_after_utc < expires:
                raise ValueError("Device CA expires too soon")
            certificate = (
                x509.CertificateBuilder()
                .subject_name(x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Xedoc device")]))
                .issuer_name(authority.subject).public_key(key)
                .serial_number(x509.random_serial_number())
                .not_valid_before(now - timedelta(minutes=5)).not_valid_after(expires)
                .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
                .add_extension(
                    x509.ExtendedKeyUsage([ExtendedKeyUsageOID.CLIENT_AUTH]), critical=True
                )
                .sign(authority_key, hashes.SHA256())
            )
            der = certificate.public_bytes(serialization.Encoding.DER)
            connection.execute(
                "INSERT INTO devices VALUES (?, ?, ?)",
                (hashlib.sha256(der).hexdigest(), name, expires.timestamp()),
            )
            connection.execute("UPDATE invite SET expires=0 WHERE id=1")
        return {"certificate": base64.b64encode(der).decode()}

    def allowed(self, certificate: bytes | None) -> bool:
        if not certificate:
            return False
        with self._connect() as connection:
            return connection.execute(
                "SELECT 1 FROM devices WHERE fingerprint=? AND expires>?",
                (hashlib.sha256(certificate).hexdigest(), time.time()),
            ).fetchone() is not None

    def devices(self) -> list[dict]:
        with self._connect() as connection:
            return [
                {"fingerprint": row[0], "name": row[1], "expiresAt": int(row[2])}
                for row in connection.execute(
                    "SELECT fingerprint, name, expires FROM devices WHERE expires>? LIMIT 128",
                    (time.time(),),
                )
            ]

    def revoke(self, fingerprint: str):
        if len(fingerprint) != 64 or any(char not in "0123456789abcdef" for char in fingerprint):
            raise ValueError("Expected a lowercase SHA-256 certificate fingerprint")
        with self._connect() as connection:
            connection.execute("DELETE FROM devices WHERE fingerprint=?", (fingerprint,))


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(prog="xedoc-remote-agentd device-pairing")
    parser.add_argument("action", choices=("invite", "list", "revoke"))
    parser.add_argument("fingerprint", nargs="?")
    parser.add_argument("--xedoc-home", type=Path, default=Path(
        os.environ.get("XEDOC_HOME", Path.home() / ".xedoc")
    ))
    parser.add_argument("--server-ca", type=Path)
    args = parser.parse_args(argv)
    try:
        pairing = HostDevicePairing(args.xedoc_home)
        if args.action == "invite":
            config = HostGuiConfig.load(args.xedoc_home)
            invitation = pairing.invite(
                config, args.server_ca or args.xedoc_home / "host-gui" / "ca-cert.pem"
            )
            config = replace(config, tls_client_ca=pairing.ca_path.resolve())
            config.create_tls_context()
            config.save(args.xedoc_home)
            print(invitation)
            fingerprint = x509.load_pem_x509_certificate(
                config.tls_cert.read_bytes()
            ).fingerprint(hashes.SHA256()).hex(":")
            print(f"Host: {config.origin}", file=sys.stderr)
            print(f"Host certificate SHA-256: {fingerprint}", file=sys.stderr)
            print("Restart Host GUI to apply device certificate protection.", file=sys.stderr)
        elif args.action == "list":
            print(json.dumps(pairing.devices()))
        else:
            if args.fingerprint is None:
                parser.error("revoke requires a certificate fingerprint")
            pairing.revoke(args.fingerprint)
    except (OSError, ValueError, ssl.SSLError, InvalidSignature, x509.ExtensionNotFound) as error:
        parser.error(str(error))
    return 0
