"""Install and supervise the outbound, transport-authenticated bridge connector."""

from datetime import datetime, timezone
import hashlib
from http.client import HTTPConnection, HTTPException
from http.cookies import SimpleCookie
import io
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import stat
import subprocess
import tarfile
import time
from types import SimpleNamespace
from urllib.parse import urlsplit
from urllib.request import urlopen

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import serialization
from cryptography.x509.oid import ExtendedKeyUsageOID

from .host_bridge_setup import _addresses, _port_state, bridge_setup, check_bridge_route
from .host_gui_config import _write_file


_VERSION = "0.72.0"
_ARCHIVES = {
    ("Darwin", "x86_64"): (
        "darwin_amd64",
        "60439dc6bf794c4182a1a4cac147d642f839d55201607208979eb23aecd09be9",
    ),
    ("Darwin", "arm64"): (
        "darwin_arm64",
        "6e8576c5b6b9914a86850f6ff4fb81c5a09e706f91f70f64d5f398558bdc9db8",
    ),
    ("Linux", "x86_64"): (
        "linux_amd64",
        "daf719b32ca2778770bb92955e231a07414d754378b9af328aa2642862479804",
    ),
    ("Linux", "aarch64"): (
        "linux_arm64",
        "02180b857480302fd934e9ddbd50fe2f73229f99062f88756abc6de9df5e95cf",
    ),
}


def _private_directory(path):
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    info = path.lstat()
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or info.st_mode & 0o077
    ):
        raise ValueError(f"Bridge directory must be owner-private: {path}")


def _credentials(home, ca_path, cert_path, key_path):
    directory = home / "bridge" / "transport"
    sources = (
        ca_path or directory / "bridge-ca.crt",
        cert_path or directory / "host.crt",
        key_path or directory / "host.key",
    )
    material = []
    for source in sources:
        source = Path(source).expanduser()
        if not source.is_file() or source.stat().st_size > 64 * 1024:
            raise ValueError(
                f"Missing or oversized transport credential: {source}. "
                "Supply --transport-ca, --transport-cert and --transport-key from "
                "the trusted bridge owner; never copy the transport CA private key."
            )
        material.append(source.read_bytes())
    ca_pem, cert_pem, key_pem = material
    try:
        if b"PRIVATE KEY" in ca_pem or b"PRIVATE KEY" in cert_pem:
            raise ValueError("Certificate files must not contain private keys")
        ca = x509.load_pem_x509_certificate(ca_pem)
        certificate = x509.load_pem_x509_certificate(cert_pem)
        key = serialization.load_pem_private_key(key_pem, password=None)
        certificate.verify_directly_issued_by(ca)
        now = datetime.now(timezone.utc)
        if (
            not ca.extensions.get_extension_for_class(x509.BasicConstraints).value.ca
            or certificate.extensions.get_extension_for_class(
                x509.BasicConstraints
            ).value.ca
            or ExtendedKeyUsageOID.CLIENT_AUTH
            not in certificate.extensions.get_extension_for_class(
                x509.ExtendedKeyUsage
            ).value
            or any(
                not item.not_valid_before_utc <= now < item.not_valid_after_utc
                for item in (ca, certificate)
            )
            or key.public_key().public_bytes(
                serialization.Encoding.DER,
                serialization.PublicFormat.SubjectPublicKeyInfo,
            )
            != certificate.public_key().public_bytes(
                serialization.Encoding.DER,
                serialization.PublicFormat.SubjectPublicKeyInfo,
            )
        ):
            raise ValueError("Invalid transport identity")
    except (ValueError, TypeError, InvalidSignature, x509.ExtensionNotFound) as error:
        raise ValueError(
            "Transport credentials are invalid, expired, or mismatched; obtain a "
            "valid client certificate/key and public transport CA from the bridge owner."
        ) from error
    return material


def _run(arguments):
    try:
        result = subprocess.run(
            arguments, capture_output=True, text=True, timeout=30, check=False
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        raise ValueError(
            f"Could not run {Path(arguments[0]).name}; check its installation/service manager"
        ) from error
    if result.returncode:
        raise ValueError(
            f"{Path(arguments[0]).name} {arguments[1]} failed: {result.stderr.strip()[:512]}"
        )
    return result.stdout.strip()


def _service(home):
    suffix = hashlib.sha256(os.fsencode(home)).hexdigest()[:12]
    label = f"com.xedoc.bridge-connector-{suffix}"
    if platform.system() == "Darwin" and shutil.which("launchctl"):
        for domain in (f"gui/{os.getuid()}", f"user/{os.getuid()}"):
            result = subprocess.run(
                ["launchctl", "print", domain],
                capture_output=True,
                timeout=15,
                check=False,
            )
            if result.returncode == 0:
                return label, domain
        raise ValueError(
            "No launchd user domain is available; sign in locally and rerun setup"
        )
    if platform.system() == "Linux" and shutil.which("systemctl"):
        _run(["systemctl", "--user", "show-environment"])
        return label, "systemd"
    raise ValueError(
        "Bridge setup requires macOS launchctl or a running Linux systemd user session; "
        "no nonpersistent connector was started."
    )


def _install_frpc(directory):
    target = _ARCHIVES.get((platform.system(), platform.machine()))
    if target is None:
        raise ValueError("frpc setup supports macOS/Linux on amd64/arm64 only")
    name, expected = target
    stem = f"frp_{_VERSION}_{name}"
    archive = directory / f"{stem}.tar.gz"
    content = archive.read_bytes() if archive.exists() else b""
    if hashlib.sha256(content).hexdigest() != expected:
        with urlopen(
            f"https://github.com/fatedier/frp/releases/download/v{_VERSION}/{stem}.tar.gz",
            timeout=60,
        ) as response:
            content = response.read(64 * 1024 * 1024 + 1)
        if (
            len(content) > 64 * 1024 * 1024
            or hashlib.sha256(content).hexdigest() != expected
        ):
            raise ValueError("Downloaded frpc archive failed its pinned SHA-256 check")
        _write_file(archive, content)
    with tarfile.open(fileobj=io.BytesIO(content), mode="r:gz") as package:
        member = package.getmember(f"{stem}/frpc")
        if not member.isfile() or member.size > 64 * 1024 * 1024:
            raise ValueError("The pinned frpc archive has no valid connector binary")
        binary = package.extractfile(member).read()
    destination = directory / f"frpc-{_VERSION}"
    if not destination.exists() or destination.read_bytes() != binary:
        _write_file(destination, binary, 0o755)
    return destination


def _reload_listener(home, config, expected):
    if _port_state(_addresses("127.0.0.1"), config.port, expected) == "ready":
        return
    try:
        path = home / "host-gui" / "local.json"
        info = path.lstat()
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_mode & 0o077
            or info.st_size > 4096
        ):
            raise ValueError("Local Host GUI descriptor must be owner-private")
        local = json.loads(path.read_text())
        origin, token = local["origin"], local["token"]
        parsed = urlsplit(origin)
        if (
            parsed.scheme != "http"
            or parsed.hostname != "127.0.0.1"
            or not parsed.port
            or parsed.netloc != f"127.0.0.1:{parsed.port}"
            or parsed.path
            or parsed.query
            or parsed.fragment
            or not isinstance(token, str)
            or not 32 <= len(token) <= 256
            or not token.isascii()
            or any(char.isspace() for char in token)
        ):
            raise ValueError("Invalid local Host GUI descriptor")
        connection = HTTPConnection("127.0.0.1", parsed.port, timeout=10)
        try:
            connection.request("GET", "/", headers={"Authorization": f"Bearer {token}"})
            response = connection.getresponse()
            cookie = SimpleCookie(response.getheader("Set-Cookie", ""))
            response.read(64 * 1024)
            if response.status != 200 or "xedoc_host" not in cookie:
                raise ValueError("Local Host GUI authentication failed")
            connection.request(
                "POST",
                "/api/device/bridge-reload",
                b"{}",
                {
                    "Cookie": f"xedoc_host={cookie['xedoc_host'].value}",
                    "Origin": origin,
                    "Content-Type": "application/json",
                },
            )
            response = connection.getresponse()
            result = json.loads(response.read(4096))
            if response.status != 200 or result != {
                "enabled": True,
                "origin": config.origin,
            }:
                raise ValueError("Local Host GUI could not reload the bridge")
        finally:
            connection.close()
    except (OSError, ValueError, KeyError, TypeError, HTTPException) as error:
        raise ValueError(
            "The bridge listener is not active. Start/update this host's Host GUI "
            "to a version supporting bridge setup, then rerun device-pairing setup. "
            "Direct access was not restarted."
        ) from error
    if _port_state(_addresses("127.0.0.1"), config.port, expected) != "ready":
        raise ValueError(
            "Host GUI reloaded, but its bridge listener certificate did not match"
        )


def _start_service(service, binary, config, directory):
    label, domain = service
    if domain == "systemd":
        path = Path.home() / ".config" / "systemd" / "user" / f"{label}.service"
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        command = " ".join(
            json.dumps(str(item), ensure_ascii=False)
            .replace("%", "%%")
            .replace("$", "$$")
            for item in (binary, "-c", config)
        )
        _write_file(
            path,
            (
                "[Unit]\nDescription=Xedoc bridge connector\n"
                f"[Service]\nExecStart={command}\nRestart=always\nRestartSec=5\n"
                "[Install]\nWantedBy=default.target\n"
            ).encode(),
        )
        _run(["systemctl", "--user", "daemon-reload"])
        _run(["systemctl", "--user", "enable", f"{label}.service"])
        _run(["systemctl", "--user", "restart", f"{label}.service"])
    else:
        path = Path.home() / "Library" / "LaunchAgents" / f"{label}.plist"
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        _write_file(
            path,
            plistlib.dumps(
                {
                    "Label": label,
                    "ProgramArguments": [str(binary), "-c", str(config)],
                    "RunAtLoad": True,
                    "KeepAlive": True,
                    "ThrottleInterval": 10,
                    "StandardOutPath": str(directory / "frpc.log"),
                    "StandardErrorPath": str(directory / "frpc.log"),
                }
            ),
        )
        subprocess.run(
            ["launchctl", "bootout", f"{domain}/{label}"],
            capture_output=True,
            timeout=15,
            check=False,
        )
        _run(["launchctl", "bootstrap", domain, str(path)])


def _owns_service(home, service):
    label, domain = service
    if domain == "systemd":
        path = Path.home() / ".config" / "systemd" / "user" / f"{label}.service"
        config = home / "bridge" / "connector" / "frpc.toml"
        binary = home / "bridge" / "connector" / f"frpc-{_VERSION}"
        try:
            info = path.lstat()
            content = path.read_text()
        except FileNotFoundError:
            return False
        command = " ".join(
            json.dumps(str(item), ensure_ascii=False)
            .replace("%", "%%")
            .replace("$", "$$")
            for item in (binary, "-c", config)
        )
        if (
            not stat.S_ISREG(info.st_mode)
            or info.st_uid != os.getuid()
            or info.st_mode & 0o077
            or "Description=Xedoc bridge connector" not in content
            or f"ExecStart={command}" not in content
        ):
            return False
        result = subprocess.run(
            ["systemctl", "--user", "is-active", "--quiet", f"{label}.service"],
            capture_output=True,
            timeout=15,
            check=False,
        )
        return result.returncode == 0
    path = Path.home() / "Library" / "LaunchAgents" / f"{label}.plist"
    try:
        info = path.lstat()
        configuration = plistlib.loads(path.read_bytes())
    except FileNotFoundError:
        return False
    config = home / "bridge" / "connector" / "frpc.toml"
    binary = home / "bridge" / "connector" / f"frpc-{_VERSION}"
    if not (
        stat.S_ISREG(info.st_mode)
        and info.st_uid == os.getuid()
        and not info.st_mode & 0o077
        and configuration.get("Label") == label
        and configuration.get("ProgramArguments", [])[-1:] == [str(config)]
        and configuration.get("ProgramArguments", [])[:1] == [str(binary)]
    ):
        return False
    result = subprocess.run(
        ["launchctl", "print", f"{domain}/{label}"],
        capture_output=True,
        text=True,
        timeout=15,
        check=False,
    )
    return result.returncode == 0 and any(
        line.strip() == "state = running" for line in result.stdout.splitlines()
    )


def setup_connector(home, origin=None, ca_path=None, cert_path=None, key_path=None):
    from .host_device_pairing import HostDevicePairing, _bridge_config
    from .host_gui_bridge import load_bridge_config

    home = home.expanduser().resolve()
    credentials = _credentials(home, ca_path, cert_path, key_path)
    service = _service(home)
    existing = load_bridge_config(home)
    directory = home / "bridge" / "connector"
    transport = home / "bridge" / "transport"
    if existing is not None and origin in (None, existing.origin):
        route = SimpleNamespace(
            origin=existing.origin, tls_certificate=existing.tls_cert.read_bytes()
        )
        if check_bridge_route(route) == "ready":
            installed = tuple(
                (transport / name).read_bytes()
                if (transport / name).is_file()
                else None
                for name in ("bridge-ca.crt", "host.crt", "host.key")
            )
            if installed == tuple(credentials):
                return {
                    "status": "ready",
                    "origin": existing.origin,
                    "service": "existing connector",
                }
            if not _owns_service(home, service):
                raise ValueError(
                    "The bridge route is already active through another connector. "
                    "Transport credentials were not changed; update that connector "
                    "using its owner-managed setup."
                )
    if origin is None and existing is None:
        origin = bridge_setup(None)["suggestedOrigin"]
        if origin is None:
            raise ValueError(
                "No unused sendnix bridge port found; configure --bridge-origin explicitly"
            )
    _private_directory(directory)
    _private_directory(transport)
    binary = _install_frpc(directory)
    pairing = HostDevicePairing(home)
    config = _bridge_config(home, pairing, origin)
    for name, content in zip(("bridge-ca.crt", "host.crt", "host.key"), credentials):
        _write_file(transport / name, content)
    public = urlsplit(config.origin)
    identity = hashlib.sha256(config.tls_cert.read_bytes()).hexdigest()[:12]
    proxy = f"xedoc-{identity}-{config.port}"
    values = {
        "serverAddr": public.hostname,
        "serverPort": 7000,
        "transport.tls.enable": True,
        "transport.tls.serverName": public.hostname,
        "transport.tls.trustedCaFile": str(transport / "bridge-ca.crt"),
        "transport.tls.certFile": str(transport / "host.crt"),
        "transport.tls.keyFile": str(transport / "host.key"),
    }
    content = "".join(f"{key} = {json.dumps(value)}\n" for key, value in values.items())
    content += (
        f'\n[[proxies]]\nname = "{proxy}"\ntype = "tcp"\n'
        f'localIP = "127.0.0.1"\nlocalPort = {config.port}\nremotePort = {config.port}\n'
    )
    candidate = directory / "frpc.pending.toml"
    _write_file(candidate, content.encode())
    _run([str(binary), "verify", "-c", str(candidate)])
    pairing._authority()
    config.create_tls_context()
    config.save(home / "bridge")
    leaf = config.tls_cert.read_bytes()
    expected = x509.load_pem_x509_certificate(leaf).public_bytes(
        serialization.Encoding.DER
    )
    _reload_listener(home, config, expected)
    config_path = directory / "frpc.toml"
    _write_file(config_path, content.encode())
    candidate.unlink()
    _start_service(service, binary, config_path, directory)
    route = SimpleNamespace(origin=config.origin, tls_certificate=leaf)
    deadline = time.monotonic() + 20
    while True:
        state = check_bridge_route(route)
        if state == "ready":
            return {"status": "ready", "origin": config.origin, "service": service[0]}
        if time.monotonic() >= deadline:
            raise ValueError(
                f"Connector service installed, but the public bridge route is {state}. "
                "Check relay TCP 7000, port allocation and connector logs; rerun setup."
            )
        time.sleep(1)
