"""Resolve address hints without treating TCP reachability as peer identity."""

import secrets
import socket
import ssl
import time

from .errors import BrokerError
from .peer_identity import certificate_from_der
from .peer_protocol import PeerEndpoint


MAX_RESOLVED_ADDRESSES = 16


def connect_peer(
    endpoint: PeerEndpoint,
    context: ssl.SSLContext,
    *,
    host_id: str,
    fingerprint: str,
    timeout_seconds: float,
) -> ssl.SSLSocket:
    """Try resolved addresses through pinned TLS, never replaying a request."""

    deadline = time.monotonic() + timeout_seconds
    try:
        resolved = socket.getaddrinfo(
            endpoint.host, endpoint.port, type=socket.SOCK_STREAM
        )
    except (OSError, UnicodeError, ValueError) as error:
        raise BrokerError.unavailable() from error
    addresses = list(dict.fromkeys(resolved))[:MAX_RESOLVED_ADDRESSES]
    failure = BrokerError.unavailable()
    for index, (family, kind, protocol, _, address) in enumerate(addresses):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        connection = None
        try:
            connection = socket.socket(family, kind, protocol)
            attempt_deadline = time.monotonic() + remaining / (len(addresses) - index)
            connection.settimeout(max(0.001, attempt_deadline - time.monotonic()))
            connection.connect(address)
            connection.settimeout(max(0.001, attempt_deadline - time.monotonic()))
            connection = context.wrap_socket(connection, server_hostname=endpoint.host)
            certificate = certificate_from_der(connection.getpeercert(binary_form=True))
            if not (
                secrets.compare_digest(certificate.host_id, host_id)
                and secrets.compare_digest(certificate.fingerprint, fingerprint)
            ):
                raise BrokerError.unauthorized()
            connection.settimeout(max(0.001, deadline - time.monotonic()))
            return connection
        except BrokerError as error:
            failure = error
        except (OSError, UnicodeError, ValueError):
            failure = BrokerError.unavailable()
        if connection is not None:
            connection.close()
    raise failure
