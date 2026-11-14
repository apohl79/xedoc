"""Enable app-managed device access on an already configured HTTPS listener."""

from dataclasses import replace
import ssl

from .host_device_pairing import HostDevicePairing


def can_invite_device(server) -> bool:
    client_ca = server.tls_config.tls_client_ca
    return (
        server.secure
        and server.tls_context is not None
        and (
            client_ca is None
            or client_ca.resolve()
            == (server.auth.directory / "device-pairing" / "ca.pem").resolve()
        )
    )


def invite_device(server, except_connection=None) -> dict:
    with server.pairing_lock:
        if not can_invite_device(server):
            raise ValueError(
                "Device invitations require private-CA HTTPS with app-managed client "
                "certificates; the existing custom client CA will not be replaced"
            )
        config = server.tls_config
        if config.tls_cert.read_bytes() != server.tls_certificate:
            raise ValueError(
                "The server certificate changed; restart Host GUI before pairing"
            )
        pairing = server.device_pairing
        if pairing is not None:
            if (
                ssl.PEM_cert_to_DER_cert(pairing.ca_path.read_text())
                not in server.tls_context.get_ca_certs(binary_form=True)
            ):
                raise ValueError(
                    "The running client CA does not match app-managed device pairing; "
                    "restart Host GUI before pairing"
                )
            return pairing.invite_details(config, server.auth.directory / "ca-cert.pem")

        pairing = HostDevicePairing(server.auth.directory.parent)
        invitation = pairing.invite_details(config, server.auth.directory / "ca-cert.pem")
        config = replace(config, tls_client_ca=pairing.ca_path)
        context = config.create_tls_context()
        if config.tls_cert.read_bytes() != server.tls_certificate:
            raise ValueError(
                "The server certificate changed; restart Host GUI before pairing"
            )
        config.save(server.auth.directory.parent)
        server.tls_context = context
        server.tls_config = config
        server.device_pairing = pairing
        server.close_existing_connections(except_connection)
        return invitation
