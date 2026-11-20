"""Issue invitations on the listener protected by the deployment policy."""

import ssl

from .host_bridge_setup import check_bridge_route


def pairing_target(server):
    if server.tls_config.network_mode == "external":
        return server
    return server.bridge_access.server


def can_invite_device(server) -> bool:
    return pairing_target(server) is not None


def invite_device(server) -> dict:
    with server.pairing_lock:
        if not can_invite_device(server):
            raise ValueError("Configure and start the sendnix bridge before inviting a device")
        target = pairing_target(server)
        if getattr(target, "bridge_only", False) and check_bridge_route(target) != "ready":
            raise ValueError(
                "The public bridge does not present this host's certificate. "
                "Choose Check again in Pair Device to check the route and find an unused port."
            )
        config = target.tls_config
        if config.tls_cert.read_bytes() != target.tls_certificate:
            raise ValueError(
                "The server certificate changed; restart Host GUI before pairing"
            )
        pairing = target.device_pairing
        if (
            ssl.PEM_cert_to_DER_cert(pairing.ca_path.read_text())
            not in target.tls_context.get_ca_certs(binary_form=True)
        ):
            raise ValueError(
                "The running listener client CA changed; restart Host GUI before pairing"
            )
        return pairing.invite_details(config, config.tls_cert.parent / "ca-cert.pem")
