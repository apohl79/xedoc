"""Explicit deployment policy for the direct Host GUI listener."""

from dataclasses import replace
import json
from pathlib import Path
import sys

from cryptography import x509
from cryptography.hazmat.primitives import hashes

from .host_device_pairing import HostDevicePairing
from .host_gui_config import HostGuiConfig


def network_tls_config(config: HostGuiConfig, home: Path) -> HostGuiConfig:
    config.validate()
    if config.network_mode != "external":
        return config
    pairing = HostDevicePairing(home)
    pairing._authority()
    return replace(config, tls_client_ca=pairing.ca_path.resolve())


def network_command(home: Path, mode: str | None) -> None:
    config = HostGuiConfig.load(home)
    if mode is not None:
        config = replace(config, network_mode=mode)
        network_tls_config(config, home).create_tls_context()
        config.save(home)
        print(
            "Saved network mode. Restart Host GUI to apply it; "
            "the running listener is unchanged.",
            file=sys.stderr,
        )
    print(json.dumps({"networkMode": config.network_mode, "source": "saved"}))


def device_invitation(home: Path, server_ca: Path | None) -> dict:
    config = HostGuiConfig.load(home)
    if config.network_mode != "external":
        raise ValueError(
            "Direct device invitations require external network mode; "
            "use device-pairing invite for a bridge"
        )
    network_tls_config(config, home).create_tls_context()
    invitation = HostDevicePairing(home).invite_details(
        config, server_ca or config.tls_cert.parent / "ca-cert.pem"
    )
    fingerprint = x509.load_pem_x509_certificate(
        config.tls_cert.read_bytes()
    ).fingerprint(hashes.SHA256()).hex(":")
    print(f"Host: {config.origin}", file=sys.stderr)
    print(f"Host certificate SHA-256: {fingerprint}", file=sys.stderr)
    return invitation
