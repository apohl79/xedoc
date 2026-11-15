"""Issue invitations on the bridge without changing direct host access."""

import ssl

from .host_bridge_setup import check_bridge_route

def can_invite_device(server) -> bool:
    return server.bridge_access.server is not None


def invite_device(server) -> dict:
    with server.pairing_lock:
        if not can_invite_device(server):
            raise ValueError("Configure and start the sendnix bridge before inviting a device")
        bridge = server.bridge_access.server
        if check_bridge_route(bridge) != "ready":
            raise ValueError(
                "The public bridge does not present this host's certificate. "
                "Choose Check again in Pair Device to check the route and find an unused port."
            )
        config = bridge.tls_config
        if config.tls_cert.read_bytes() != bridge.tls_certificate:
            raise ValueError(
                "The server certificate changed; restart Host GUI before pairing"
            )
        pairing = bridge.device_pairing
        if (
            ssl.PEM_cert_to_DER_cert(pairing.ca_path.read_text())
            not in bridge.tls_context.get_ca_certs(binary_form=True)
        ):
            raise ValueError(
                "The running bridge client CA changed; restart Host GUI before pairing"
            )
        return pairing.invite_details(config, config.tls_cert.parent / "ca-cert.pem")
