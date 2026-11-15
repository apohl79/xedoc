"""Read-only relay observations; a suggested port is not a reservation."""

from concurrent.futures import ThreadPoolExecutor
import socket
import ssl
from urllib.parse import urlsplit

from cryptography import x509
from cryptography.hazmat.primitives import serialization


def _addresses(host):
    selected = {}
    try:
        for family, _, _, _, address in socket.getaddrinfo(
            host, None, type=socket.SOCK_STREAM
        ):
            if family in (socket.AF_INET, socket.AF_INET6):
                selected.setdefault(family, address)
    except OSError:
        pass
    return list(selected.items())


def _port_state(addresses, port, expected=b""):
    if not addresses:
        return "unknown"
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    states = []
    for family, address in addresses:
        try:
            with socket.socket(family, socket.SOCK_STREAM) as connection:
                connection.settimeout(1)
                connection.connect((address[0], port, *address[2:]))
                try:
                    # No credentials or requests: inspect the leaf, never grant trust.
                    with context.wrap_socket(connection) as protected:
                        leaf = protected.getpeercert(binary_form=True)
                except (ssl.SSLEOFError, ConnectionResetError):
                    # An unclaimed, Docker-published frp port closes after TCP connect.
                    states.append("unused")
                    continue
                if not expected or leaf != expected:
                    return "occupied"
                states.append("ready")
        except ConnectionRefusedError:
            states.append("unused")
        except OSError:
            return "unknown"
    return states[0] if len(set(states)) == 1 else "unknown"


def check_bridge_route(bridge):
    origin = urlsplit(bridge.origin)
    expected = x509.load_pem_x509_certificate(bridge.tls_certificate).public_bytes(
        serialization.Encoding.DER
    )
    state = _port_state(_addresses(origin.hostname), origin.port or 443, expected)
    return {"ready": "ready", "occupied": "mismatch"}.get(state, "unreachable")


def bridge_setup(bridge):
    origin = bridge.origin if bridge is not None else None
    status = check_bridge_route(bridge) if bridge is not None else "unconfigured"
    result = {"status": status, "origin": origin, "suggestedOrigin": None}
    if status == "ready":
        return result
    addresses = _addresses("sendnix.de")
    current_port = urlsplit(origin).port if origin is not None else None
    ports = [port for port in range(50001, 50011) if port != current_port]
    with ThreadPoolExecutor(max_workers=10) as executor:
        states = executor.map(lambda port: _port_state(addresses, port), ports)
        for port, state in zip(ports, states):
            if state == "unused":
                result["suggestedOrigin"] = f"https://sendnix.de:{port}"
                break
    return result
