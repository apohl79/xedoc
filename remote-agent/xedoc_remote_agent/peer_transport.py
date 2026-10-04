"""TLS 1.3 transport for signed peer frames; no app-server traffic enters here."""

from __future__ import annotations

import secrets
import socket
import ssl
import threading
from typing import Any, Mapping

from .errors import BrokerError
from .models import Role
from .peer_identity import CertificateMaterial, HostIdentity, certificate_from_der
from .peer_protocol import (
    CONTROL_KINDS,
    PEER_OPERATIONS,
    PeerEndpoint,
    PeerTarget,
    frame_limit,
    parse_response,
    read_frame,
    request_id_or_empty,
    signed_request,
    timeout,
    write_frame,
)
from .peer_state import PeerState


MAX_PEER_CLIENTS = 16
PEER_IO_TIMEOUT_SECONDS = 5.0


class PeerServer:
    """A bounded direct TLS 1.3 listener for authenticated broker frames."""

    def __init__(
        self,
        service: Any,
        endpoint: PeerEndpoint,
        *,
        max_request_bytes: int,
        max_response_bytes: int,
        bootstrap: bool = False,
    ) -> None:
        self._service = service
        self._endpoint = endpoint
        self._max_request_bytes = frame_limit(max_request_bytes)
        self._max_response_bytes = frame_limit(max_response_bytes)
        self._bootstrap = bootstrap
        self._listener: socket.socket | None = None
        self._thread: threading.Thread | None = None
        self._stopping = threading.Event()
        self._clients = threading.BoundedSemaphore(MAX_PEER_CLIENTS)
        self._workers: set[threading.Thread] = set()
        self._connections: set[socket.socket] = set()
        self._lock = threading.Lock()

    @property
    def endpoint(self) -> str:
        return self._endpoint.value

    @property
    def running(self) -> bool:
        return self._listener is not None

    def start(self) -> None:
        if self._listener is not None:
            raise BrokerError.conflict()
        listener = _create_listener(self._endpoint)
        listener.listen(MAX_PEER_CLIENTS)
        listener.settimeout(0.2)
        self._endpoint = PeerEndpoint(
            host=self._endpoint.host, port=int(listener.getsockname()[1])
        )
        self._listener = listener
        self._stopping.clear()
        self._thread = threading.Thread(
            target=self._serve,
            name="xedoc-remote-agent-peer",
            daemon=True,
        )
        self._thread.start()

    def stop(self) -> None:
        self._stopping.set()
        listener, self._listener = self._listener, None
        if listener is not None:
            try:
                listener.close()
            except OSError:
                pass
        with self._lock:
            connections = tuple(self._connections)
            workers = tuple(self._workers)
        for connection in connections:
            try:
                connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            try:
                connection.close()
            except OSError:
                pass
        for worker in workers:
            worker.join(timeout=1.0)
        thread, self._thread = self._thread, None
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=1.0)

    def _serve(self) -> None:
        while not self._stopping.is_set():
            listener = self._listener
            if listener is None:
                return
            try:
                connection, _ = listener.accept()
            except TimeoutError:
                continue
            except OSError:
                if self._stopping.is_set():
                    return
                continue
            if not self._clients.acquire(blocking=False):
                connection.close()
                continue
            worker = threading.Thread(
                target=self._handle_connection,
                args=(connection,),
                name="xedoc-remote-agent-peer-client",
                daemon=True,
            )
            with self._lock:
                self._workers.add(worker)
                self._connections.add(connection)
            worker.start()

    def _handle_connection(self, connection: socket.socket) -> None:
        tls_connection: ssl.SSLSocket | None = None
        try:
            connection.settimeout(PEER_IO_TIMEOUT_SECONDS)
            context = (
                self._service.bootstrap_tls_context()
                if self._bootstrap
                else self._service.server_tls_context()
            )
            tls_connection = context.wrap_socket(connection, server_side=True)
            request = read_frame(tls_connection, self._max_request_bytes)
            if self._bootstrap:
                response = self._service.handle_bootstrap_pairing_request(request)
            else:
                peer_der = tls_connection.getpeercert(binary_form=True)
                if not isinstance(peer_der, bytes):
                    raise BrokerError.unauthorized()
                certificate = certificate_from_der(peer_der)
                response = self._service.handle_peer_request(request, certificate)
            write_frame(tls_connection, response, self._max_response_bytes)
        except (BrokerError, OSError, ssl.SSLError, TimeoutError):
            if tls_connection is not None:
                try:
                    response = self._service.error_response(
                        request_id=request_id_or_empty(locals().get("request")),
                        error=BrokerError.unauthorized(),
                    )
                    write_frame(tls_connection, response, self._max_response_bytes)
                except (BrokerError, OSError, ssl.SSLError):
                    pass
        finally:
            try:
                if tls_connection is not None:
                    tls_connection.close()
                else:
                    connection.close()
            except OSError:
                pass
            self._clients.release()
            with self._lock:
                self._workers.discard(threading.current_thread())
                self._connections.discard(connection)


class PeerClient:
    """One-request-per-connection mTLS client pinned to a peer certificate."""

    def __init__(
        self,
        identity: HostIdentity,
        role: Role,
        state: PeerState,
        *,
        max_request_bytes: int,
        max_response_bytes: int,
    ) -> None:
        self._identity = identity
        self._role = role
        self._state = state
        self._max_request_bytes = frame_limit(max_request_bytes)
        self._max_response_bytes = frame_limit(max_response_bytes)

    def operation(
        self,
        peer: PeerTarget,
        operation: str,
        params: Mapping[str, Any],
        *,
        timeout_seconds: float,
    ) -> Mapping[str, Any]:
        if operation not in PEER_OPERATIONS or not isinstance(params, Mapping):
            raise BrokerError.invalid_request()
        return self._exchange(
            peer,
            kind="operation",
            value={"operation": operation, "params": dict(params)},
            timeout_seconds=timeout_seconds,
        )

    def control(
        self,
        peer: PeerTarget,
        kind: str,
        payload: Mapping[str, Any],
        *,
        timeout_seconds: float,
    ) -> Mapping[str, Any]:
        if kind not in CONTROL_KINDS or not isinstance(payload, Mapping):
            raise BrokerError.invalid_request()
        return self._exchange(
            peer,
            kind=kind,
            value={"payload": dict(payload)},
            timeout_seconds=timeout_seconds,
        )

    def bootstrap_pairing(
        self,
        endpoint: PeerEndpoint,
        *,
        host_id: str,
        fingerprint: str,
        role: Role,
        payload: Mapping[str, Any],
        timeout_seconds: float,
    ) -> tuple[Mapping[str, Any], CertificateMaterial]:
        """Send one pairing request after pinning a discovered server identity."""

        if (
            not isinstance(endpoint, PeerEndpoint)
            or not isinstance(host_id, str)
            or not host_id
            or not isinstance(fingerprint, str)
            or not isinstance(role, Role)
            or not isinstance(payload, Mapping)
        ):
            raise BrokerError.invalid_request()
        request_timeout = timeout(timeout_seconds)
        request_id = f"req_{secrets.token_urlsafe(18)}"
        request = signed_request(
            self._identity,
            self._role,
            request_id=request_id,
            kind="pairing",
            value={"payload": dict(payload)},
            timeout_seconds=request_timeout,
        )
        context = bootstrap_client_tls_context()
        try:
            with socket.create_connection(
                (endpoint.host, endpoint.port),
                timeout=request_timeout,
            ) as raw_connection:
                raw_connection.settimeout(request_timeout)
                with context.wrap_socket(
                    raw_connection,
                    server_hostname=endpoint.host,
                ) as connection:
                    certificate = certificate_from_der(
                        connection.getpeercert(binary_form=True)
                    )
                    if (
                        not secrets.compare_digest(certificate.host_id, host_id)
                        or not secrets.compare_digest(
                            certificate.fingerprint, fingerprint
                        )
                    ):
                        raise BrokerError.unauthorized()
                    write_frame(connection, request, self._max_request_bytes)
                    response = read_frame(connection, self._max_response_bytes)
        except BrokerError:
            raise
        except (OSError, ssl.SSLError, TimeoutError) as error:
            raise BrokerError.unavailable() from error
        return (
            parse_response(response, request_id, certificate, expected_role=role),
            certificate,
        )

    def _exchange(
        self,
        peer: PeerTarget,
        *,
        kind: str,
        value: Mapping[str, Any],
        timeout_seconds: float,
    ) -> Mapping[str, Any]:
        request_timeout = timeout(timeout_seconds)
        request_id = f"req_{secrets.token_urlsafe(18)}"
        request = signed_request(
            self._identity,
            self._role,
            request_id=request_id,
            kind=kind,
            value=value,
            timeout_seconds=request_timeout,
        )
        context = client_tls_context(self._state, peer.certificate)
        try:
            with socket.create_connection(
                (peer.endpoint.host, peer.endpoint.port),
                timeout=request_timeout,
            ) as raw_connection:
                raw_connection.settimeout(request_timeout)
                with context.wrap_socket(
                    raw_connection,
                    server_hostname=peer.endpoint.host,
                ) as connection:
                    certificate = certificate_from_der(
                        connection.getpeercert(binary_form=True)
                    )
                    if not secrets.compare_digest(
                        certificate.fingerprint, peer.certificate.fingerprint
                    ):
                        raise BrokerError.unauthorized()
                    write_frame(connection, request, self._max_request_bytes)
                    response = read_frame(connection, self._max_response_bytes)
        except BrokerError:
            raise
        except (OSError, ssl.SSLError, TimeoutError) as error:
            raise BrokerError.unavailable() from error
        return parse_response(
            response,
            request_id,
            peer.certificate,
            expected_role=peer.role,
        )


def server_tls_context(
    state: PeerState,
    certificates: list[CertificateMaterial],
) -> ssl.SSLContext:
    """Build a TLS 1.3 listener context requiring an eligible client cert."""

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_3
    context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(
        certfile=str(state.certificate_path),
        keyfile=str(state.key_path),
    )
    certificate_data = _certificate_bundle(certificates)
    if certificate_data:
        try:
            context.load_verify_locations(cadata=certificate_data)
        except ssl.SSLError as error:
            raise BrokerError.invalid_request() from error
    context.verify_mode = ssl.CERT_REQUIRED
    context.check_hostname = False
    return context


def bootstrap_server_tls_context(state: PeerState) -> ssl.SSLContext:
    """Build the server-authenticated TLS context for pairing only."""

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_3
    context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.load_cert_chain(
        certfile=str(state.certificate_path),
        keyfile=str(state.key_path),
    )
    context.verify_mode = ssl.CERT_NONE
    context.check_hostname = False
    return context


def client_tls_context(
    state: PeerState,
    certificate: CertificateMaterial,
) -> ssl.SSLContext:
    """Build a TLS 1.3 client context pinned to precisely one peer cert."""

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.minimum_version = ssl.TLSVersion.TLSv1_3
    context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.check_hostname = False
    context.verify_mode = ssl.CERT_REQUIRED
    try:
        context.load_verify_locations(cadata=certificate.pem.decode("ascii"))
    except (UnicodeError, ssl.SSLError) as error:
        raise BrokerError.invalid_request() from error
    context.load_cert_chain(
        certfile=str(state.certificate_path),
        keyfile=str(state.key_path),
    )
    return context


def bootstrap_client_tls_context() -> ssl.SSLContext:
    """Build an unverified client context before explicit identity pinning."""

    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.minimum_version = ssl.TLSVersion.TLSv1_3
    context.maximum_version = ssl.TLSVersion.TLSv1_3
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    return context


def _create_listener(endpoint: PeerEndpoint) -> socket.socket:
    listener: socket.socket | None = None
    try:
        family = socket.AF_INET6 if ":" in endpoint.host else socket.AF_INET
        listener = socket.socket(family, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((endpoint.host, endpoint.port))
        return listener
    except OSError as error:
        if listener is not None:
            listener.close()
        raise BrokerError.unavailable() from error


def _certificate_bundle(certificates: list[CertificateMaterial]) -> str:
    seen: set[str] = set()
    values: list[str] = []
    for certificate in certificates:
        if certificate.fingerprint in seen:
            continue
        seen.add(certificate.fingerprint)
        try:
            values.append(certificate.pem.decode("ascii"))
        except UnicodeError as error:
            raise BrokerError.invalid_request() from error
    return "\n".join(values)
