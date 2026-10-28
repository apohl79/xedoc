"""Explicit, single-use enrollment into a shared Host GUI certificate authority."""

import base64
from datetime import datetime, timedelta, timezone
import hashlib
from http.client import HTTPSConnection
import ipaddress
import json
import plistlib
from pathlib import Path
import secrets
import sqlite3
import socket
import ssl
import time
from urllib.parse import urlsplit
import uuid

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID

from .host_gui_config import _write_file


def approved_names(text: str) -> list[x509.GeneralName]:
    names = []
    for value in text.split(","):
        value = value.strip().lower()
        try:
            name = x509.IPAddress(ipaddress.ip_address(value))
        except ValueError:
            value = value.encode("idna").decode("ascii")
            if (
                not value
                or len(value) > 253
                or any(
                    not label
                    or len(label) > 63
                    or label.startswith("-")
                    or label.endswith("-")
                    or any(not (char.isalnum() or char == "-") for char in label)
                    for label in value.split(".")
                )
            ):
                raise ValueError("Enter exact DNS names or IP addresses, not wildcards")
            name = x509.DNSName(value)
        if name not in names:
            names.append(name)
    if not 1 <= len(names) <= 16:
        raise ValueError("Approve between one and 16 certificate names")
    return names


class HostCA:
    def __init__(self, home: Path):
        self.directory = home / "host-gui"

    def certificate(self) -> bytes:
        return (self.directory / "ca-cert.pem").read_bytes()

    def fingerprint(self) -> str:
        certificate = x509.load_pem_x509_certificate(self.certificate())
        return certificate.fingerprint(hashes.SHA256()).hex(":").upper()

    def existing_server_identity(self, hostname: str) -> tuple[Path, Path, Path]:
        certificate_path = self.directory / "server-cert.pem"
        key_path = self.directory / "server-key.pem"
        ca_path = self.directory / "ca-cert.pem"
        certificate = x509.load_pem_x509_certificate(certificate_path.read_bytes())
        authority = x509.load_pem_x509_certificate(ca_path.read_bytes())
        key = serialization.load_pem_private_key(key_path.read_bytes(), password=None)
        expected_name = approved_names(hostname)[0]
        try:
            subject_names = certificate.extensions.get_extension_for_class(
                x509.SubjectAlternativeName
            ).value
            certificate.verify_directly_issued_by(authority)
        except (InvalidSignature, x509.ExtensionNotFound, ValueError) as error:
            raise ValueError(
                "Existing shared CA server certificate is invalid"
            ) from error
        if (
            expected_name not in subject_names
            or certificate.public_key().public_bytes(
                serialization.Encoding.DER,
                serialization.PublicFormat.SubjectPublicKeyInfo,
            )
            != key.public_key().public_bytes(
                serialization.Encoding.DER,
                serialization.PublicFormat.SubjectPublicKeyInfo,
            )
            or not certificate.not_valid_before_utc
            <= datetime.now(timezone.utc)
            < certificate.not_valid_after_utc
        ):
            raise ValueError(
                "Existing shared CA certificate is invalid for this HTTPS origin"
            )
        return certificate_path, key_path, ca_path

    def mobileconfig(self) -> bytes:
        certificate = x509.load_pem_x509_certificate(self.certificate())
        fingerprint = certificate.fingerprint(hashes.SHA256()).hex()
        identifier = f"com.xedoc.host-gui.ca.{fingerprint}"
        profile_uuid = uuid.uuid5(uuid.NAMESPACE_URL, identifier)
        certificate_uuid = uuid.uuid5(profile_uuid, "certificate")
        return plistlib.dumps(
            {
                "PayloadContent": [
                    {
                        "PayloadCertificateFileName": "xedoc-lan-ca.crt",
                        "PayloadContent": certificate.public_bytes(
                            serialization.Encoding.DER
                        ),
                        "PayloadDescription": "Trust the Xedoc private CA for HTTPS access.",
                        "PayloadDisplayName": "Xedoc LAN CA",
                        "PayloadIdentifier": f"{identifier}.certificate",
                        "PayloadType": "com.apple.security.root",
                        "PayloadUUID": str(certificate_uuid),
                        "PayloadVersion": 1,
                    }
                ],
                "PayloadDescription": "Install the Xedoc CA for trusted HTTPS access.",
                "PayloadDisplayName": "Xedoc LAN CA",
                "PayloadIdentifier": f"{identifier}.profile",
                "PayloadOrganization": "Xedoc",
                "PayloadRemovalDisallowed": False,
                "PayloadType": "Configuration",
                "PayloadUUID": str(profile_uuid),
                "PayloadVersion": 1,
            },
            fmt=plistlib.FMT_XML,
            sort_keys=False,
        )

    def invite(self, origin: str, names: str) -> str:
        approved = approved_names(names)
        certificate = self.certificate()
        # Only a signing host can create invitations.
        serialization.load_pem_private_key(
            (self.directory / "ca-key.pem").read_bytes(), password=None
        )
        token = secrets.token_urlsafe(32)
        database = self.directory / "ca-invitations.sqlite3"
        with sqlite3.connect(database) as connection:
            connection.execute("BEGIN IMMEDIATE")
            connection.execute(
                "CREATE TABLE IF NOT EXISTS invitations "
                "(token TEXT PRIMARY KEY, names TEXT NOT NULL, expires INTEGER NOT NULL)"
            )
            connection.execute(
                "DELETE FROM invitations WHERE expires <= ?", (int(time.time()),)
            )
            if (
                connection.execute("SELECT COUNT(*) FROM invitations").fetchone()[0]
                >= 32
            ):
                raise ValueError("Too many pending CA invitations; wait for expiry")
            connection.execute(
                "INSERT INTO invitations VALUES (?, ?, ?)",
                (
                    hashlib.sha256(token.encode()).hexdigest(),
                    ",".join(str(name.value) for name in approved),
                    int(time.time()) + 600,
                ),
            )
        database.chmod(0o600)
        return base64.urlsafe_b64encode(
            json.dumps(
                {"origin": origin, "ca": certificate.decode(), "token": token},
                separators=(",", ":"),
            ).encode()
        ).decode()

    def enroll(self, token: str, csr_pem: str) -> dict:
        if not isinstance(token, str) or not isinstance(csr_pem, str):
            raise ValueError("Expected enrollment code and certificate request")
        database = self.directory / "ca-invitations.sqlite3"
        if not database.exists():
            raise PermissionError("Invalid or expired CA invitation")
        csr = x509.load_pem_x509_csr(csr_pem.encode())
        if not csr.is_signature_valid or not isinstance(
            csr.public_key(), ec.EllipticCurvePublicKey
        ):
            raise ValueError("Expected a signed EC certificate request")
        if any(
            not isinstance(extension.value, x509.SubjectAlternativeName)
            for extension in csr.extensions
        ):
            raise ValueError("Certificate request may only contain approved names")
        try:
            requested = csr.extensions.get_extension_for_class(
                x509.SubjectAlternativeName
            ).value
        except x509.ExtensionNotFound as error:
            raise ValueError(
                "Certificate request must include approved names"
            ) from error
        if any(
            not isinstance(name, (x509.DNSName, x509.IPAddress)) for name in requested
        ):
            raise ValueError("Only DNS and IP certificate names are supported")
        with sqlite3.connect(database) as connection:
            connection.execute("BEGIN IMMEDIATE")
            digest = hashlib.sha256(token.encode()).hexdigest()
            invitation = connection.execute(
                "SELECT names FROM invitations WHERE token = ? AND expires > ?",
                (digest, int(time.time())),
            ).fetchone()
            if invitation is None:
                raise PermissionError("Invalid or expired CA invitation")
            if set(requested) != set(approved_names(invitation[0])):
                raise PermissionError("Certificate names do not match the invitation")
            certificate, authority_pem = self._issue_certificate(csr, list(requested))
            connection.execute("DELETE FROM invitations WHERE token = ?", (digest,))
        return {
            "certificate": certificate.public_bytes(
                serialization.Encoding.PEM
            ).decode(),
            "ca": authority_pem.decode(),
        }

    def enroll_discovered(self, csr_pem: str, source_address: str) -> dict:
        if not isinstance(csr_pem, str) or len(csr_pem) > 16 * 1024:
            raise ValueError("Expected a bounded certificate request")
        requester = _normalized_ip(source_address)
        if requester is None:
            raise PermissionError("Invalid enrollment source address")
        csr = x509.load_pem_x509_csr(csr_pem.encode())
        if (
            not csr.is_signature_valid
            or not isinstance(csr.public_key(), ec.EllipticCurvePublicKey)
            or len(csr.extensions) != 1
            or not isinstance(csr.extensions[0].value, x509.SubjectAlternativeName)
        ):
            raise ValueError("Expected a signed EC request containing only host names")
        requested = csr.extensions[0].value
        if not 1 <= len(requested) <= 4:
            raise ValueError("A host certificate must contain one to four names")
        approved = []
        for name in requested:
            if isinstance(name, x509.DNSName):
                normalized = approved_names(name.value)
                if len(normalized) != 1 or not isinstance(normalized[0], x509.DNSName):
                    raise ValueError(
                        "Only exact DNS names or IP addresses are supported"
                    )
                try:
                    resolved = {
                        _normalized_ip(result[4][0])
                        for result in socket.getaddrinfo(
                            name.value, 0, type=socket.SOCK_STREAM
                        )
                    }
                except OSError as error:
                    raise PermissionError(
                        "Requested DNS names must resolve to the connecting host"
                    ) from error
                if requester not in resolved:
                    raise PermissionError(
                        "Requested DNS names must resolve to the connecting host"
                    )
                approved.extend(normalized)
            elif isinstance(name, x509.IPAddress):
                address = _normalized_ip(str(name.value))
                if address != requester:
                    raise PermissionError(
                        "Requested IP names must match the connecting host"
                    )
                approved.append(x509.IPAddress(ipaddress.ip_address(address)))
            else:
                raise ValueError("Only exact DNS names or IP addresses are supported")
        certificate, authority_pem = self._issue_certificate(csr, approved)
        return {
            "certificate": certificate.public_bytes(
                serialization.Encoding.PEM
            ).decode(),
            "ca": authority_pem.decode(),
        }

    def _issue_certificate(
        self, csr: x509.CertificateSigningRequest, names: list[x509.GeneralName]
    ) -> tuple[x509.Certificate, bytes]:
        authority_pem = self.certificate()
        authority = x509.load_pem_x509_certificate(authority_pem)
        key = serialization.load_pem_private_key(
            (self.directory / "ca-key.pem").read_bytes(), password=None
        )
        if authority.public_key().public_bytes(
            serialization.Encoding.DER,
            serialization.PublicFormat.SubjectPublicKeyInfo,
        ) != key.public_key().public_bytes(
            serialization.Encoding.DER,
            serialization.PublicFormat.SubjectPublicKeyInfo,
        ):
            raise ValueError("Coordinator CA certificate and key do not match")
        now = datetime.now(timezone.utc)
        if not authority.not_valid_before_utc <= now < authority.not_valid_after_utc:
            raise ValueError("Coordinator CA has expired or is not yet valid")
        certificate = (
            x509.CertificateBuilder()
            .subject_name(
                x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Xedoc Host GUI")])
            )
            .issuer_name(authority.subject)
            .public_key(csr.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - timedelta(minutes=5))
            .not_valid_after(
                min(now + timedelta(days=397), authority.not_valid_after_utc)
            )
            .add_extension(
                x509.BasicConstraints(ca=False, path_length=None), critical=True
            )
            .add_extension(x509.SubjectAlternativeName(names), critical=False)
            .add_extension(
                x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]),
                critical=False,
            )
            .sign(key, hashes.SHA256())
        )
        return certificate, authority_pem


def join_authority(home: Path, invitation: str, names: str) -> tuple[Path, Path, Path]:
    directory = home / "host-gui"
    if (directory / "ca-key.pem").exists():
        raise ValueError(
            "This host already owns a CA; refusing to replace its trust identity"
        )
    if len(invitation) > 16384:
        raise ValueError("CA invitation exceeds size limit")
    value = json.loads(base64.b64decode(invitation, altchars=b"-_", validate=True))
    if not isinstance(value, dict) or any(
        not isinstance(value.get(field), str) for field in ("origin", "ca", "token")
    ):
        raise ValueError("Invalid CA invitation")
    origin = urlsplit(value["origin"])
    if (
        origin.scheme != "https"
        or not origin.hostname
        or origin.username
        or origin.password
        or origin.path
        or origin.query
        or origin.fragment
    ):
        raise ValueError("CA invitation must use an exact HTTPS origin")
    ca_pem = value["ca"]
    authority = x509.load_pem_x509_certificate(ca_pem.encode())
    try:
        is_ca = authority.extensions.get_extension_for_class(
            x509.BasicConstraints
        ).value.ca
    except x509.ExtensionNotFound:
        is_ca = False
    if not is_ca:
        raise ValueError("Invitation certificate is not a CA")
    key = ec.generate_private_key(ec.SECP256R1())
    csr = (
        x509.CertificateSigningRequestBuilder()
        .subject_name(
            x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Xedoc Host GUI")])
        )
        .add_extension(
            x509.SubjectAlternativeName(approved_names(names)), critical=False
        )
        .sign(key, hashes.SHA256())
    )
    context = ssl.create_default_context(cadata=ca_pem)
    connection = HTTPSConnection(
        origin.hostname, origin.port or 443, context=context, timeout=15
    )
    try:
        connection.request(
            "POST",
            "/api/ca/enroll",
            json.dumps(
                {
                    "token": value["token"],
                    "csr": csr.public_bytes(serialization.Encoding.PEM).decode(),
                }
            ),
            {"Content-Type": "application/json", "Origin": value["origin"]},
        )
        response = connection.getresponse()
        data = response.read(16385)
        if response.status != 200 or len(data) > 16384:
            raise ValueError(
                "CA enrollment rejected; check the invitation and approved names"
            )
        result = json.loads(data)
    finally:
        connection.close()
    certificate = x509.load_pem_x509_certificate(result["certificate"].encode())
    certificate.verify_directly_issued_by(authority)
    if (
        certificate.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )
        != key.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )
        or set(
            certificate.extensions.get_extension_for_class(
                x509.SubjectAlternativeName
            ).value
        )
        != set(approved_names(names))
        or certificate.extensions.get_extension_for_class(
            x509.BasicConstraints
        ).value.ca
    ):
        raise ValueError("Coordinator returned an unexpected certificate")
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    cert_path, key_path, ca_path = (
        directory / name
        for name in ("server-cert.pem", "server-key.pem", "ca-cert.pem")
    )
    _write_file(
        key_path,
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        ),
    )
    _write_file(cert_path, certificate.public_bytes(serialization.Encoding.PEM), 0o644)
    _write_file(ca_path, ca_pem.encode(), 0o644)
    return cert_path, key_path, ca_path


def join_discovered_authority(
    home: Path,
    origin_value: str,
    expected_fingerprint: str,
    challenge: str,
    client_address: str,
    names: str,
) -> tuple[Path, Path, Path]:
    directory = home / "host-gui"
    if (directory / "ca-key.pem").exists() or (directory / "ca-cert.pem").exists():
        raise ValueError(
            "This host already has a CA identity; refusing to replace its trust identity"
        )
    origin = urlsplit(origin_value)
    if (
        origin.scheme != "https"
        or not origin.hostname
        or origin.username
        or origin.password
        or origin.path
        or origin.query
        or origin.fragment
    ):
        raise ValueError("Coordinator must advertise an exact HTTPS origin")
    if not isinstance(expected_fingerprint, str) or not _is_fingerprint(
        expected_fingerprint
    ):
        raise ValueError("Invalid coordinator CA fingerprint")
    if not isinstance(challenge, str) or not 32 <= len(challenge) <= 128:
        raise ValueError("Invalid or expired CA discovery challenge")
    client_ip = _normalized_ip(client_address)
    if client_ip is None or ":" in client_ip:
        raise ValueError("CA discovery requires a local IPv4 network address")

    fetch_context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    fetch_context.check_hostname = False
    fetch_context.verify_mode = ssl.CERT_NONE
    connection = HTTPSConnection(
        origin.hostname,
        origin.port or 443,
        timeout=10,
        source_address=(client_ip, 0),
        context=fetch_context,
    )
    try:
        connection.request("GET", "/ca.crt")
        response = connection.getresponse()
        ca_pem = response.read(16 * 1024 + 1)
        if response.status != 200 or len(ca_pem) > 16 * 1024:
            raise ValueError("Coordinator did not provide a bounded public CA")
    finally:
        connection.close()
    authority = x509.load_pem_x509_certificate(ca_pem)
    actual_fingerprint = authority.fingerprint(hashes.SHA256()).hex(":").upper()
    if not secrets.compare_digest(actual_fingerprint, expected_fingerprint.upper()):
        raise PermissionError(
            "Coordinator CA did not match the fingerprint shown during discovery"
        )
    try:
        is_ca = authority.extensions.get_extension_for_class(
            x509.BasicConstraints
        ).value.ca
    except x509.ExtensionNotFound:
        is_ca = False
    if not is_ca:
        raise ValueError("Discovered certificate is not a CA")

    key = ec.generate_private_key(ec.SECP256R1())
    approved = approved_names(names)
    csr = (
        x509.CertificateSigningRequestBuilder()
        .subject_name(
            x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Xedoc Host GUI")])
        )
        .add_extension(x509.SubjectAlternativeName(approved), critical=False)
        .sign(key, hashes.SHA256())
    )
    context = ssl.create_default_context(cadata=ca_pem.decode())
    connection = HTTPSConnection(
        origin.hostname,
        origin.port or 443,
        timeout=15,
        source_address=(client_ip, 0),
        context=context,
    )
    try:
        connection.request(
            "POST",
            "/api/ca/enroll/discovered",
            json.dumps(
                {
                    "challenge": challenge,
                    "csr": csr.public_bytes(serialization.Encoding.PEM).decode(),
                }
            ),
            {"Content-Type": "application/json", "Origin": origin_value},
        )
        response = connection.getresponse()
        data = response.read(16385)
        if response.status != 200 or len(data) > 16384:
            raise ValueError("Coordinator rejected automatic CA enrollment")
        result = json.loads(data)
    finally:
        connection.close()
    certificate = x509.load_pem_x509_certificate(result["certificate"].encode())
    certificate.verify_directly_issued_by(authority)
    if (
        certificate.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )
        != key.public_key().public_bytes(
            serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo
        )
        or set(
            certificate.extensions.get_extension_for_class(
                x509.SubjectAlternativeName
            ).value
        )
        != set(approved)
        or certificate.extensions.get_extension_for_class(
            x509.BasicConstraints
        ).value.ca
    ):
        raise ValueError("Coordinator returned an unexpected certificate")
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    cert_path, key_path, ca_path = (
        directory / name
        for name in ("server-cert.pem", "server-key.pem", "ca-cert.pem")
    )
    _write_file(
        key_path,
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        ),
    )
    _write_file(cert_path, certificate.public_bytes(serialization.Encoding.PEM), 0o644)
    _write_file(ca_path, ca_pem, 0o644)
    return cert_path, key_path, ca_path


def _normalized_ip(value: str) -> str | None:
    try:
        address = ipaddress.ip_address(value.split("%", 1)[0])
    except (AttributeError, ValueError):
        return None
    if isinstance(address, ipaddress.IPv6Address) and address.ipv4_mapped:
        address = address.ipv4_mapped
    return str(address)


def _is_fingerprint(value: str) -> bool:
    compact = value.replace(":", "")
    return (
        len(value) == 95
        and len(compact) == 64
        and all(character in "0123456789abcdefABCDEF" for character in compact)
    )
