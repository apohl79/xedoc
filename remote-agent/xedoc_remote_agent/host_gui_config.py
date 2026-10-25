"""Persisted Host GUI listener settings and their TLS validation."""

from dataclasses import asdict, dataclass
from datetime import datetime, timedelta, timezone
import ipaddress
import json
import os
from pathlib import Path
import ssl
import tempfile
from urllib.parse import urlsplit

from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID


@dataclass(frozen=True)
class HostGuiConfig:
    listen_address: str
    port: int
    origin: str
    tls_cert: Path | None = None
    tls_key: Path | None = None

    @classmethod
    def local_only(cls) -> "HostGuiConfig":
        return cls("127.0.0.1", 50000, "http://127.0.0.1:50000")

    @classmethod
    def load(cls, home: Path) -> "HostGuiConfig":
        path = home / "host-gui" / "server.json"
        if not path.exists():
            return cls.local_only()
        if path.stat().st_size > 16 * 1024:
            raise ValueError("Host GUI settings exceed size limit")
        value = json.loads(path.read_text())
        if not isinstance(value, dict):
            raise ValueError("Host GUI settings must be an object")
        config = cls(
            listen_address=value["listen_address"],
            port=value["port"],
            origin=value["origin"],
            tls_cert=Path(value["tls_cert"]) if value.get("tls_cert") else None,
            tls_key=Path(value["tls_key"]) if value.get("tls_key") else None,
        )
        config.validate()
        return config

    def validate(self) -> None:
        if not isinstance(self.origin, str):
            raise ValueError("Host GUI origin must be a string")
        origin = urlsplit(self.origin)
        if (
            origin.scheme not in ("http", "https")
            or not origin.netloc
            or origin.path
            or origin.query
            or origin.fragment
            or origin.username
            or origin.password
        ):
            raise ValueError("Host GUI origin must be an exact HTTP or HTTPS origin")
        if (
            not isinstance(self.listen_address, str)
            or not self.listen_address
            or not isinstance(self.port, int)
            or isinstance(self.port, bool)
            or not 1 <= self.port <= 65535
        ):
            raise ValueError("Invalid Host GUI listener address or port")
        if origin.scheme == "http":
            if self.tls_cert or self.tls_key:
                raise ValueError("TLS certificate paths require an HTTPS origin")
            if self.listen_address not in ("127.0.0.1", "::1", "localhost"):
                raise ValueError("Plain HTTP Host GUI listeners must be local-only")
        elif not self.tls_cert or not self.tls_key:
            raise ValueError("HTTPS requires a TLS certificate chain and private key")
        if (
            origin.scheme == "https"
            and origin.port == 443
            and origin.netloc.endswith(":443")
        ):
            raise ValueError("HTTPS origins must omit the default :443 port")
        if (
            origin.scheme == "https"
            and ":" in (origin.hostname or "")
            and ":" not in self.listen_address
        ):
            raise ValueError("IPv6 HTTPS origins require an IPv6 listener address")
        if (origin.port or (443 if origin.scheme == "https" else 80)) != self.port:
            raise ValueError("Host GUI listener port must match the origin port")
        if (self.tls_cert and not self.tls_cert.is_absolute()) or (
            self.tls_key and not self.tls_key.is_absolute()
        ):
            raise ValueError("Host GUI TLS paths must be absolute")

    def create_tls_context(self) -> ssl.SSLContext | None:
        self.validate()
        if self.tls_cert is None or self.tls_key is None:
            return None
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        context.load_cert_chain(self.tls_cert, self.tls_key)
        return context

    def save(self, home: Path) -> None:
        self.validate()
        directory = home / "host-gui"
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        path = directory / "server.json"
        value = {
            **asdict(self),
            "tls_cert": str(self.tls_cert) if self.tls_cert else None,
            "tls_key": str(self.tls_key) if self.tls_key else None,
        }
        descriptor, temporary = tempfile.mkstemp(dir=directory)
        try:
            with os.fdopen(descriptor, "w") as stream:
                json.dump(value, stream)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    @staticmethod
    def generate_tls_material(home: Path, hostname: str) -> tuple[Path, Path, Path]:
        directory = home / "host-gui"
        directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(directory, 0o700)

        now = datetime.now(timezone.utc).replace(tzinfo=None)
        authority_key_path = directory / "ca-key.pem"
        authority_cert_path = directory / "ca-cert.pem"
        if authority_key_path.exists() != authority_cert_path.exists():
            raise ValueError(
                "Only one Host GUI CA file exists; remove both CA files to rotate it"
            )
        if authority_key_path.exists():
            authority_key = serialization.load_pem_private_key(
                authority_key_path.read_bytes(),
                password=None,
            )
            authority = x509.load_pem_x509_certificate(
                authority_cert_path.read_bytes()
            )
            try:
                is_ca = authority.extensions.get_extension_for_class(
                    x509.BasicConstraints
                ).value.ca
            except x509.ExtensionNotFound:
                is_ca = False
            public_format = serialization.PublicFormat.SubjectPublicKeyInfo
            encoding = serialization.Encoding.DER
            if (
                not is_ca
                or authority.subject != authority.issuer
                or authority.public_key().public_bytes(encoding, public_format)
                != authority_key.public_key().public_bytes(encoding, public_format)
                or not authority.not_valid_before_utc
                <= datetime.now(timezone.utc)
                <= authority.not_valid_after_utc
            ):
                raise ValueError("Existing Host GUI CA material is invalid")
        else:
            authority_key = ec.generate_private_key(ec.SECP256R1())
            authority_name = x509.Name(
                [x509.NameAttribute(NameOID.COMMON_NAME, "Xedoc Host GUI private CA")]
            )
            authority = (
                x509.CertificateBuilder()
                .subject_name(authority_name)
                .issuer_name(authority_name)
                .public_key(authority_key.public_key())
                .serial_number(x509.random_serial_number())
                .not_valid_before(now - timedelta(minutes=5))
                .not_valid_after(now + timedelta(days=3650))
                .add_extension(
                    x509.BasicConstraints(ca=True, path_length=0),
                    critical=True,
                )
                .add_extension(
                    x509.KeyUsage(
                        digital_signature=True,
                        content_commitment=False,
                        key_encipherment=False,
                        data_encipherment=False,
                        key_agreement=False,
                        key_cert_sign=True,
                        crl_sign=True,
                        encipher_only=None,
                        decipher_only=None,
                    ),
                    critical=True,
                )
                .sign(authority_key, hashes.SHA256())
            )
            _write_file(
                authority_key_path,
                authority_key.private_bytes(
                    serialization.Encoding.PEM,
                    serialization.PrivateFormat.PKCS8,
                    serialization.NoEncryption(),
                ),
            )
            _write_file(
                authority_cert_path,
                authority.public_bytes(serialization.Encoding.PEM),
                0o644,
            )

        try:
            subject_alt_name: x509.GeneralName = x509.IPAddress(
                ipaddress.ip_address(hostname)
            )
        except ValueError:
            subject_alt_name = x509.DNSName(hostname)
        server_key = ec.generate_private_key(ec.SECP256R1())
        server_name = x509.Name(
            [x509.NameAttribute(NameOID.COMMON_NAME, "Xedoc Host GUI")]
        )
        server_certificate = (
            x509.CertificateBuilder()
            .subject_name(server_name)
            .issuer_name(authority.subject)
            .public_key(server_key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - timedelta(minutes=5))
            .not_valid_after(now + timedelta(days=397))
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .add_extension(
                x509.SubjectAlternativeName([subject_alt_name]),
                critical=False,
            )
            .add_extension(
                x509.ExtendedKeyUsage([ExtendedKeyUsageOID.SERVER_AUTH]),
                critical=False,
            )
            .sign(authority_key, hashes.SHA256())
        )

        server_key_path = directory / "server-key.pem"
        server_cert_path = directory / "server-cert.pem"
        _write_file(
            server_key_path,
            server_key.private_bytes(
                serialization.Encoding.PEM,
                serialization.PrivateFormat.PKCS8,
                serialization.NoEncryption(),
            ),
        )
        _write_file(
            server_cert_path,
            server_certificate.public_bytes(serialization.Encoding.PEM),
            0o644,
        )
        return server_cert_path, server_key_path, authority_cert_path


def _write_file(path: Path, content: bytes, mode: int = 0o600) -> None:
    descriptor, temporary_name = tempfile.mkstemp(dir=path.parent, prefix=f".{path.name}.")
    temporary = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.chmod(temporary, mode)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)
