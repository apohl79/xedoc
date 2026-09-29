"""Ed25519 identity and X.509 certificate handling for broker peers."""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
import hashlib
import os
from pathlib import Path
import secrets
import stat
from types import SimpleNamespace
from urllib.parse import quote_from_bytes, unquote_to_bytes

from .errors import BrokerError
from .models import MAX_ID_LENGTH


MAX_CERTIFICATE_BYTES = 128 * 1024
HOST_ID_URI_PREFIX = "urn:xedoc:host:"
MAX_HOST_ID_URI_BYTES = 2048


@dataclass(frozen=True)
class CertificateMaterial:
    """Canonical public certificate material safe to pin in local state."""

    pem: bytes
    fingerprint: str
    host_id: str


@dataclass(frozen=True)
class HostIdentity:
    """The persistent Ed25519 identity used for TLS and peer signatures."""

    host_id: str
    private_key_pem: bytes
    certificate: CertificateMaterial


def load_certificate(path: str | os.PathLike[str]) -> CertificateMaterial:
    """Load exactly one bounded Ed25519 X.509 PEM from local configuration."""

    certificate_path = Path(path)
    if not certificate_path.is_absolute():
        raise BrokerError.invalid_request()
    return certificate_material(_read_certificate(certificate_path))


def certificate_material(data: bytes) -> CertificateMaterial:
    """Canonicalize one PEM certificate and require its bound host identity."""

    if not isinstance(data, bytes) or not 0 < len(data) <= MAX_CERTIFICATE_BYTES:
        raise BrokerError.invalid_request()
    crypto = _crypto()
    try:
        certificate = crypto.x509.load_pem_x509_certificate(data)
        return _certificate_material(certificate)
    except (TypeError, UnicodeError, ValueError) as error:
        raise BrokerError.invalid_request() from error


def certificate_from_der(value: bytes) -> CertificateMaterial:
    """Canonicalize a peer certificate read from a successful TLS session."""

    crypto = _crypto()
    try:
        certificate = crypto.x509.load_der_x509_certificate(value)
        return _certificate_material(certificate)
    except (TypeError, UnicodeError, ValueError) as error:
        raise BrokerError.unauthorized() from error


def new_identity(host_id: str) -> HostIdentity:
    """Create one self-signed TLS 1.3 Ed25519 host identity."""

    validate_host_id(host_id)
    crypto = _crypto()
    try:
        private_key = crypto.ed25519.Ed25519PrivateKey.generate()
        now = datetime.now(timezone.utc)
        subject = issuer = crypto.x509.Name(
            [crypto.x509.NameAttribute(crypto.NameOID.COMMON_NAME, "xedoc-remote-agent")]
        )
        certificate = (
            crypto.x509.CertificateBuilder()
            .subject_name(subject)
            .issuer_name(issuer)
            .public_key(private_key.public_key())
            .serial_number(crypto.x509.random_serial_number())
            .not_valid_before(now - timedelta(minutes=5))
            .not_valid_after(now + timedelta(days=3650))
            .add_extension(
                crypto.x509.BasicConstraints(ca=True, path_length=None),
                critical=True,
            )
            .add_extension(
                crypto.x509.KeyUsage(
                    digital_signature=True,
                    content_commitment=False,
                    key_encipherment=False,
                    data_encipherment=False,
                    key_agreement=False,
                    key_cert_sign=True,
                    crl_sign=False,
                    encipher_only=False,
                    decipher_only=False,
                ),
                critical=True,
            )
            .add_extension(
                crypto.x509.ExtendedKeyUsage(
                    [crypto.ExtendedKeyUsageOID.CLIENT_AUTH, crypto.ExtendedKeyUsageOID.SERVER_AUTH]
                ),
                critical=False,
            )
            .add_extension(
                crypto.x509.SubjectAlternativeName(
                    [crypto.x509.UniformResourceIdentifier(_host_id_uri(host_id))]
                ),
                critical=True,
            )
            .sign(private_key, algorithm=None)
        )
        private_key_pem = private_key.private_bytes(
            crypto.serialization.Encoding.PEM,
            crypto.serialization.PrivateFormat.PKCS8,
            crypto.serialization.NoEncryption(),
        )
        certificate_pem = certificate.public_bytes(crypto.serialization.Encoding.PEM)
        return HostIdentity(
            host_id=host_id,
            private_key_pem=private_key_pem,
            certificate=certificate_material(certificate_pem),
        )
    except (TypeError, UnicodeError, ValueError) as error:
        raise BrokerError.internal() from error


def identity_from_row(row: tuple[object, ...]) -> HostIdentity:
    """Validate an identity row before using it for TLS or signatures."""

    if len(row) != 4:
        raise BrokerError.internal()
    host_id, private_key_pem, certificate_pem, fingerprint = row
    if (
        not isinstance(host_id, str)
        or not isinstance(private_key_pem, bytes)
        or not isinstance(certificate_pem, bytes)
        or not isinstance(fingerprint, str)
    ):
        raise BrokerError.internal()
    validate_host_id(host_id)
    material = certificate_material(certificate_pem)
    if not secrets.compare_digest(material.fingerprint, fingerprint):
        raise BrokerError.internal()
    if material.host_id != host_id:
        raise BrokerError.internal()
    crypto = _crypto()
    try:
        private_key = crypto.serialization.load_pem_private_key(private_key_pem, password=None)
        if not isinstance(private_key, crypto.ed25519.Ed25519PrivateKey):
            raise ValueError
        certificate = crypto.x509.load_pem_x509_certificate(certificate_pem)
        if (
            private_key.public_key().public_bytes(
                crypto.serialization.Encoding.Raw,
                crypto.serialization.PublicFormat.Raw,
            )
            != certificate.public_key().public_bytes(
                crypto.serialization.Encoding.Raw,
                crypto.serialization.PublicFormat.Raw,
            )
        ):
            raise ValueError
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
    return HostIdentity(host_id, private_key_pem, material)


def legacy_identity_host_id_from_row(row: tuple[object, ...]) -> str:
    """Validate one exact pre-SAN identity row during the state migration."""

    if len(row) != 4:
        raise BrokerError.internal()
    host_id, private_key_pem, certificate_pem, fingerprint = row
    if (
        not isinstance(host_id, str)
        or not isinstance(private_key_pem, bytes)
        or not isinstance(certificate_pem, bytes)
        or not isinstance(fingerprint, str)
    ):
        raise BrokerError.internal()
    validate_host_id(host_id)
    if not secrets.compare_digest(
        legacy_certificate_fingerprint(certificate_pem),
        fingerprint,
    ):
        raise BrokerError.internal()
    crypto = _crypto()
    try:
        private_key = crypto.serialization.load_pem_private_key(
            private_key_pem, password=None
        )
        if not isinstance(private_key, crypto.ed25519.Ed25519PrivateKey):
            raise ValueError
        certificate = crypto.x509.load_pem_x509_certificate(certificate_pem)
        if private_key.public_key().public_bytes(
            crypto.serialization.Encoding.Raw,
            crypto.serialization.PublicFormat.Raw,
        ) != certificate.public_key().public_bytes(
            crypto.serialization.Encoding.Raw,
            crypto.serialization.PublicFormat.Raw,
        ):
            raise ValueError
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
    return host_id


def legacy_certificate_fingerprint(certificate_pem: bytes) -> str:
    """Validate one exact pre-SAN certificate and return its DER fingerprint."""

    if (
        not isinstance(certificate_pem, bytes)
        or not 0 < len(certificate_pem) <= MAX_CERTIFICATE_BYTES
    ):
        raise BrokerError.internal()
    crypto = _crypto()
    try:
        certificate = crypto.x509.load_pem_x509_certificate(certificate_pem)
        _validate_legacy_certificate(certificate)
        certificate_der = certificate.public_bytes(crypto.serialization.Encoding.DER)
    except (
        TypeError,
        ValueError,
        crypto.x509.ExtensionNotFound,
        crypto.InvalidSignature,
    ) as error:
        raise BrokerError.internal() from error
    return hashlib.sha256(certificate_der).hexdigest()


def materialize_identity(
    key_path: Path,
    certificate_path: Path,
    identity: HostIdentity,
) -> None:
    """Write TLS runtime files atomically with private permissions."""

    _write_atomic_private_file(key_path, identity.private_key_pem)
    _write_atomic_private_file(certificate_path, identity.certificate.pem)


def sign(identity: HostIdentity, payload: bytes) -> str:
    """Sign canonical payload bytes with the host identity's Ed25519 key."""

    crypto = _crypto()
    try:
        private_key = crypto.serialization.load_pem_private_key(
            identity.private_key_pem,
            password=None,
        )
        if not isinstance(private_key, crypto.ed25519.Ed25519PrivateKey):
            raise ValueError
        signature = private_key.sign(payload)
    except (TypeError, ValueError) as error:
        raise BrokerError.internal() from error
    return _base64_encode(signature)


def verify(certificate: CertificateMaterial, payload: bytes, signature: str) -> None:
    """Verify an Ed25519 signature using a pinned peer certificate."""

    crypto = _crypto()
    try:
        material = certificate_material(certificate.pem)
        if (
            not secrets.compare_digest(material.fingerprint, certificate.fingerprint)
            or material.host_id != certificate.host_id
        ):
            raise ValueError
        raw_signature = _base64_decode(signature)
        parsed = crypto.x509.load_pem_x509_certificate(material.pem)
        public_key = parsed.public_key()
        if not isinstance(public_key, crypto.ed25519.Ed25519PublicKey):
            raise ValueError
        public_key.verify(raw_signature, payload)
    except (TypeError, UnicodeError, ValueError, crypto.InvalidSignature) as error:
        raise BrokerError.unauthorized() from error


def generated_host_id() -> str:
    """Return a non-secret durable host identifier for a first install."""

    return f"host_{secrets.token_hex(16)}"


def validate_host_id(value: str) -> None:
    if (
        not isinstance(value, str)
        or not value
        or len(value) > MAX_ID_LENGTH
        or any(character.isspace() or ord(character) < 33 for character in value)
    ):
        raise BrokerError.invalid_request()


def _certificate_material(certificate: object) -> CertificateMaterial:
    crypto = _crypto()
    if not isinstance(certificate, crypto.x509.Certificate):
        raise ValueError
    if not isinstance(certificate.public_key(), crypto.ed25519.Ed25519PublicKey):
        raise ValueError
    certificate_der = certificate.public_bytes(crypto.serialization.Encoding.DER)
    return CertificateMaterial(
        pem=certificate.public_bytes(crypto.serialization.Encoding.PEM),
        fingerprint=hashlib.sha256(certificate_der).hexdigest(),
        host_id=_certificate_host_id(certificate),
    )


def _certificate_host_id(certificate: object) -> str:
    crypto = _crypto()
    if not isinstance(certificate, crypto.x509.Certificate):
        raise ValueError
    try:
        extension = certificate.extensions.get_extension_for_class(
            crypto.x509.SubjectAlternativeName
        )
    except crypto.x509.ExtensionNotFound as error:
        raise ValueError from error
    names = tuple(extension.value)
    if (
        not extension.critical
        or len(names) != 1
        or not isinstance(names[0], crypto.x509.UniformResourceIdentifier)
    ):
        raise ValueError
    uri = names[0].value
    if (
        not isinstance(uri, str)
        or not uri.startswith(HOST_ID_URI_PREFIX)
        or len(uri.encode("utf-8")) > MAX_HOST_ID_URI_BYTES
    ):
        raise ValueError
    try:
        host_id = unquote_to_bytes(uri.removeprefix(HOST_ID_URI_PREFIX)).decode("utf-8")
    except UnicodeDecodeError as error:
        raise ValueError from error
    validate_host_id(host_id)
    if _host_id_uri(host_id) != uri:
        raise ValueError
    return host_id


def _validate_legacy_certificate(certificate: object) -> None:
    crypto = _crypto()
    if not isinstance(certificate, crypto.x509.Certificate):
        raise ValueError
    if not isinstance(certificate.public_key(), crypto.ed25519.Ed25519PublicKey):
        raise ValueError
    expected_name = crypto.x509.Name(
        [crypto.x509.NameAttribute(crypto.NameOID.COMMON_NAME, "xedoc-remote-agent")]
    )
    if (
        certificate.version != crypto.x509.Version.v3
        or certificate.subject != expected_name
        or certificate.issuer != expected_name
        or len(certificate.extensions) != 3
    ):
        raise ValueError
    basic_constraints = certificate.extensions.get_extension_for_class(
        crypto.x509.BasicConstraints
    )
    if (
        not basic_constraints.critical
        or not basic_constraints.value.ca
        or basic_constraints.value.path_length is not None
    ):
        raise ValueError
    key_usage = certificate.extensions.get_extension_for_class(crypto.x509.KeyUsage)
    if (
        not key_usage.critical
        or not key_usage.value.digital_signature
        or key_usage.value.content_commitment
        or key_usage.value.key_encipherment
        or key_usage.value.data_encipherment
        or key_usage.value.key_agreement
        or not key_usage.value.key_cert_sign
        or key_usage.value.crl_sign
    ):
        raise ValueError
    extended_key_usage = certificate.extensions.get_extension_for_class(
        crypto.x509.ExtendedKeyUsage
    )
    if extended_key_usage.critical or tuple(extended_key_usage.value) != (
        crypto.ExtendedKeyUsageOID.CLIENT_AUTH,
        crypto.ExtendedKeyUsageOID.SERVER_AUTH,
    ):
        raise ValueError
    try:
        certificate.extensions.get_extension_for_class(
            crypto.x509.SubjectAlternativeName
        )
    except crypto.x509.ExtensionNotFound:
        pass
    else:
        raise ValueError
    certificate.public_key().verify(
        certificate.signature, certificate.tbs_certificate_bytes
    )


def _host_id_uri(host_id: str) -> str:
    validate_host_id(host_id)
    uri = f"{HOST_ID_URI_PREFIX}{quote_from_bytes(host_id.encode('utf-8'), safe='')}"
    if len(uri.encode("utf-8")) > MAX_HOST_ID_URI_BYTES:
        raise ValueError
    return uri


def _read_certificate(path: Path) -> bytes:
    try:
        info = path.lstat()
    except FileNotFoundError as error:
        raise BrokerError.not_found() from error
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or info.st_size <= 0
        or info.st_size > MAX_CERTIFICATE_BYTES
    ):
        raise BrokerError.invalid_request()
    flags = os.O_RDONLY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
        with os.fdopen(descriptor, "rb") as input_file:
            data = input_file.read(MAX_CERTIFICATE_BYTES + 1)
    except PermissionError as error:
        raise BrokerError.unauthorized() from error
    except OSError as error:
        raise BrokerError.unavailable() from error
    if not data or len(data) > MAX_CERTIFICATE_BYTES:
        raise BrokerError.invalid_request()
    return data


def _write_atomic_private_file(path: Path, value: bytes) -> None:
    _validate_private_target(path)
    temporary = path.with_name(f".{path.name}.{secrets.token_hex(8)}.tmp")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(temporary, flags, 0o600)
        with os.fdopen(descriptor, "wb") as output:
            output.write(value)
            output.flush()
            os.fsync(output.fileno())
        _chmod_private(temporary)
        os.replace(temporary, path)
        _chmod_private(path)
    except PermissionError as error:
        raise BrokerError.unauthorized() from error
    except OSError as error:
        raise BrokerError.unavailable() from error
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass
        except OSError:
            pass


def _validate_private_target(path: Path) -> None:
    try:
        info = path.lstat()
    except FileNotFoundError:
        return
    except OSError as error:
        raise BrokerError.unavailable() from error
    if (
        stat.S_ISLNK(info.st_mode)
        or not stat.S_ISREG(info.st_mode)
        or _wrong_owner(info)
        or (os.name != "nt" and info.st_mode & 0o077)
    ):
        raise BrokerError.unauthorized()


def _wrong_owner(info: os.stat_result) -> bool:
    return hasattr(os, "getuid") and info.st_uid != os.getuid()


def _chmod_private(path: Path) -> None:
    try:
        os.chmod(path, 0o600, follow_symlinks=False)
    except (NotImplementedError, OSError) as error:
        if os.name != "nt":
            raise BrokerError.unavailable() from error


def _base64_encode(value: bytes) -> str:
    import base64

    return base64.urlsafe_b64encode(value).decode("ascii").rstrip("=")


def _base64_decode(value: str) -> bytes:
    import base64

    if not isinstance(value, str) or not value or len(value) > 256:
        raise ValueError
    return base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))


def _crypto() -> SimpleNamespace:
    try:
        from cryptography import x509
        from cryptography.exceptions import InvalidSignature
        from cryptography.hazmat.primitives import serialization
        from cryptography.hazmat.primitives.asymmetric import ed25519
        from cryptography.x509.oid import ExtendedKeyUsageOID, NameOID
    except ImportError as error:
        raise BrokerError.unavailable("peer cryptography is unavailable") from error
    return SimpleNamespace(
        x509=x509,
        InvalidSignature=InvalidSignature,
        serialization=serialization,
        ed25519=ed25519,
        ExtendedKeyUsageOID=ExtendedKeyUsageOID,
        NameOID=NameOID,
    )
