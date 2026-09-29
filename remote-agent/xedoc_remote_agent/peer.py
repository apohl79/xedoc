"""Mutually authenticated TLS transport and Stage 4 peer read plane."""

from __future__ import annotations

from dataclasses import dataclass
import secrets
import ssl
import threading
import time
from typing import Any, Mapping

from .errors import BrokerError
from .message_contract import (
    DELIVERY_INTERRUPT,
    MESSAGE_CAPABILITY,
    MESSAGE_OPERATION,
    MessageSource,
    MessageSubmission,
    peer_message_params,
    validate_message_receipt,
    validate_peer_message,
)
from .messages import MessageService
from .models import BrokerConfig, Role, StaticPeerConfig
from .peer_identity import CertificateMaterial, HostIdentity, load_certificate
from .peer_protocol import (
    PROTOCOL,
    PROTOCOL_MAJOR,
    PROTOCOL_MINOR,
    PROTOCOL_VERSION,
    PeerEndpoint,
    PeerTarget,
    exact_fields as _exact_fields,
    fingerprint as _fingerprint,
    grants_from_payload as _grants_from_payload,
    identifier as _identifier,
    optional_identifier as _optional_identifier,
    parse_request as _parse_request,
    payload as _payload,
    request_id_or_empty as _request_id_or_empty,
    signed_response as _signed_response,
)
from .peer_session_contract import (
    SESSION_OPERATIONS,
    validate_session_params,
    validate_session_result,
)
from .peer_sessions import PeerSessionOperations
from .peer_state import (
    Grant,
    GrantPolicy,
    PeerState,
    ReadAdmission,
    Relationship,
)
from .peer_transport import PEER_IO_TIMEOUT_SECONDS, PeerClient, PeerServer, server_tls_context


StaticPeer = PeerTarget
_MAX_PENDING_REVIEWS = 64
_PENDING_REVIEW_TTL_SECONDS = 3_600


@dataclass(frozen=True)
class DiscoveredPeer:
    """A bounded discovery result derived from one static peer probe."""

    host_id: str
    role: Role
    endpoint: str
    fingerprint: str
    protocol_major: int
    protocol_minor: int
    capabilities: frozenset[str]

    def to_dict(self) -> dict[str, object]:
        return {
            "hostId": self.host_id,
            "role": self.role.value,
            "endpoint": self.endpoint,
            "fingerprint": self.fingerprint,
            "protocolVersion": self.protocol_major,
            "protocolMajor": self.protocol_major,
            "protocolMinor": self.protocol_minor,
            "capabilities": sorted(self.capabilities),
        }


@dataclass(frozen=True)
class _PendingReview:
    request_id: str
    status: str
    created_at: int
    updated_at: int

    def public_dict(self) -> dict[str, object]:
        return {
            "requestId": self.request_id,
            "status": self.status,
            "createdAt": self.created_at,
            "updatedAt": self.updated_at,
        }


class _PendingReviewRegistry:
    """Keep bounded coordinator-local review decisions out of peer transport."""

    def __init__(self) -> None:
        self._reviews: dict[str, _PendingReview] = {}
        self._lock = threading.Lock()

    def review(self, request_id: str) -> dict[str, object]:
        with self._lock:
            self._prune()
            review = self._reviews.get(request_id)
            if review is None:
                if len(self._reviews) >= _MAX_PENDING_REVIEWS:
                    raise BrokerError.limit_exceeded()
                now = int(time.time())
                review = _PendingReview(request_id, "pending", now, now)
                self._reviews[request_id] = review
            return review.public_dict()

    def decide(self, request_id: str, status: str) -> dict[str, object]:
        with self._lock:
            self._prune()
            review = self._reviews.get(request_id)
            if review is None:
                raise BrokerError.not_found()
            if review.status != "pending":
                raise BrokerError.conflict()
            decided = _PendingReview(
                request_id, status, review.created_at, int(time.time())
            )
            self._reviews[request_id] = decided
            return decided.public_dict()

    def _prune(self) -> None:
        expires_at = int(time.time()) - _PENDING_REVIEW_TTL_SECONDS
        for request_id in [
            request_id
            for request_id, review in self._reviews.items()
            if review.updated_at <= expires_at
        ]:
            del self._reviews[request_id]


class PeerService:
    """Own local pairing controls plus the grant-checked remote read plane."""

    def __init__(
        self,
        *,
        identity: HostIdentity,
        state: PeerState,
        config: BrokerConfig,
        catalog: Any,
        session_operations: PeerSessionOperations | None = None,
        message_service: MessageService | None = None,
    ) -> None:
        if not isinstance(identity, HostIdentity) or not isinstance(state, PeerState):
            raise BrokerError.invalid_request()
        self.identity = identity
        self.state = state
        self.config = config
        self.catalog = catalog
        if session_operations is not None and not isinstance(
            session_operations, PeerSessionOperations
        ):
            raise BrokerError.invalid_request()
        self._session_operations = session_operations
        if message_service is not None and not isinstance(message_service, MessageService):
            raise BrokerError.invalid_request()
        self._message_service = message_service
        self._pending_reviews = _PendingReviewRegistry()
        self._static_peers = _static_peers(config.static_peers)
        self._eligible_certificates = {
            certificate.fingerprint: certificate
            for certificate in (
                load_certificate(path)
                for path in config.managed_coordinator_certificate_paths
            )
        }
        self.state.reconcile_role(config.role)
        self._client = PeerClient(
            identity,
            config.role,
            state,
            max_request_bytes=config.limits.max_message_bytes,
            max_response_bytes=config.limits.max_result_bytes,
        )
        self._server: PeerServer | None = None

    @property
    def host_id(self) -> str:
        return self.identity.host_id

    def start_listener(self) -> PeerServer | None:
        if self.config.peer_listener is None:
            return None
        endpoint = PeerEndpoint.parse(self.config.peer_listener.endpoint)
        server = PeerServer(
            self,
            endpoint,
            max_request_bytes=self.config.limits.max_message_bytes,
            max_response_bytes=self.config.limits.max_result_bytes,
        )
        server.start()
        self._server = server
        return server

    def stop_listener(self) -> None:
        server, self._server = self._server, None
        if server is not None:
            server.stop()

    def server_tls_context(self) -> ssl.SSLContext:
        trusted = [
            *self._static_peers,
            *(
                StaticPeer(
                    endpoint=PeerEndpoint.parse(relationship.endpoint),
                    certificate=relationship.certificate,
                    role=relationship.peer_role,
                )
                for relationship in self.state.relationships()
                if relationship.status in {"pending", "paired", "suspended"}
            ),
        ]
        certificates = [
            *(peer.certificate for peer in trusted),
            *(
                load_certificate(path)
                for path in self.config.managed_coordinator_certificate_paths
            ),
        ]
        return server_tls_context(self.state, certificates)

    def hosts_list(self) -> dict[str, object]:
        return {"data": [relationship.public_dict() for relationship in self.state.relationships()]}

    def request_review(self, params: Mapping[str, Any]) -> dict[str, object]:
        _exact_fields(params, {"requestId"}, {"requestId"})
        self._require_coordinator()
        result = self._pending_reviews.review(_identifier(params["requestId"]))
        self._record_local_review("request.review", "pending")
        return result

    def request_approve(self, params: Mapping[str, Any]) -> dict[str, object]:
        _exact_fields(params, {"requestId", "reason"}, {"requestId"})
        self._require_coordinator()
        _optional_review_reason(params.get("reason"))
        result = self._pending_reviews.decide(
            _identifier(params["requestId"]), "approved"
        )
        self._record_local_review("request.approve", "approved")
        return result

    def request_reject(self, params: Mapping[str, Any]) -> dict[str, object]:
        _exact_fields(params, {"requestId", "reason"}, {"requestId"})
        self._require_coordinator()
        _optional_review_reason(params.get("reason"))
        result = self._pending_reviews.decide(
            _identifier(params["requestId"]), "rejected"
        )
        self._record_local_review("request.reject", "rejected")
        return result

    def discover(self, params: Mapping[str, Any]) -> dict[str, object]:
        _exact_fields(params, {"timeoutSeconds"})
        timeout_seconds = params.get("timeoutSeconds", self.config.limits.max_discovery_seconds)
        if (
            not isinstance(timeout_seconds, int)
            or isinstance(timeout_seconds, bool)
            or timeout_seconds < 0
            or timeout_seconds > self.config.limits.max_discovery_seconds
        ):
            raise BrokerError.limit_exceeded()
        deadline = time.monotonic() + timeout_seconds
        candidates: list[DiscoveredPeer] = []
        seen: set[str] = set()
        for peer in self._static_peers:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            try:
                response = self._client.operation(
                    peer,
                    "host/describe",
                    {},
                    timeout_seconds=min(PEER_IO_TIMEOUT_SECONDS, remaining),
                )
                candidate = _discovered_peer(response, peer)
            except BrokerError:
                continue
            if candidate.host_id not in seen:
                seen.add(candidate.host_id)
                candidates.append(candidate)
        timed_out = bool(self._static_peers) and time.monotonic() >= deadline
        return {
            "data": [candidate.to_dict() for candidate in candidates],
            "timedOut": timed_out,
        }

    def pair(self, params: Mapping[str, Any]) -> dict[str, object]:
        _exact_fields(params, {"hostId", "role", "fingerprint"}, {"hostId", "role"})
        if self.config.role is not Role.COORDINATOR:
            raise BrokerError.unauthorized()
        if self.config.peer_listener is None:
            raise BrokerError.unavailable()
        peer_host_id = _identifier(params["hostId"])
        try:
            peer_role = Role(params["role"])
        except (TypeError, ValueError) as error:
            raise BrokerError.invalid_request() from error
        fingerprint = params.get("fingerprint")
        if fingerprint is not None and not _fingerprint(fingerprint):
            raise BrokerError.invalid_request()
        candidate = self._find_candidate(
            peer_host_id,
            peer_role,
            fingerprint=fingerprint,
        )
        self.state.begin_pair(
            peer_host_id=candidate.host_id,
            peer_role=candidate.role,
            certificate=self._certificate_for_candidate(candidate),
            endpoint=candidate.endpoint,
        )
        response = self._client.control(
            self._peer_for_candidate(candidate),
            "pairing",
            {
                "action": "pair",
                "targetHostId": candidate.host_id,
                "targetRole": candidate.role.value,
                "sourceEndpoint": self._listener_endpoint(),
            },
            timeout_seconds=PEER_IO_TIMEOUT_SECONDS,
        )
        if response.get("status") != "paired":
            raise BrokerError.conflict()
        relationship = self.state.complete_pair(
            peer_host_id=candidate.host_id,
            certificate=self._certificate_for_candidate(candidate),
        )
        return relationship.public_dict()

    def grants_list(self, params: Mapping[str, Any]) -> dict[str, object]:
        _exact_fields(params, {"hostId"}, {"hostId"})
        peer_host_id = _identifier(params["hostId"])
        relationship = self.state.relationship(peer_host_id)
        if relationship is None:
            raise BrokerError.not_found()
        return {
            "hostId": peer_host_id,
            "revision": relationship.grant_revision,
            "data": [grant.public_dict() for grant in self.state.current_grants(peer_host_id)],
        }

    def grant_set(self, params: Mapping[str, Any]) -> dict[str, object]:
        _exact_fields(
            params,
            {"hostId", "scopes", "workspaceId", "threadId", "expiresAt"},
            {"hostId", "scopes"},
        )
        if self.config.role is not Role.COORDINATOR:
            raise BrokerError.unauthorized()
        peer_host_id = _identifier(params["hostId"])
        relationship = self._paired_relationship(peer_host_id)
        scopes = params["scopes"]
        if (
            not isinstance(scopes, list)
            or not scopes
            or len(scopes) > 5
            or any(
                scope
                not in {
                    "discovery",
                    "workspaceRead",
                    "sessionRead",
                    "sessionWrite",
                    "cancellation",
                }
                for scope in scopes
            )
        ):
            raise BrokerError.invalid_request()
        workspace_id = _optional_identifier(params.get("workspaceId"))
        thread_id = _optional_identifier(params.get("threadId"))
        expires_at = params.get("expiresAt")
        if expires_at is not None and (
            not isinstance(expires_at, int) or isinstance(expires_at, bool)
        ):
            raise BrokerError.invalid_request()
        grants = tuple(
            Grant(
                scope=scope,
                workspace_id=workspace_id,
                thread_id=thread_id,
                expires_at=expires_at,
            )
            for scope in sorted(set(scopes))
        )
        revision = self.state.next_grant_revision(peer_host_id)
        peer = _peer_from_relationship(relationship)
        self._client.control(
            peer,
            "grant",
            {
                "action": "replace",
                "targetHostId": peer_host_id,
                "revision": revision,
                "grants": [grant.public_dict() for grant in grants],
            },
            timeout_seconds=PEER_IO_TIMEOUT_SECONDS,
        )
        self.state.replace_grants(
            peer_host_id=peer_host_id,
            revision=revision,
            grants=grants,
            actor_host_id=self.identity.host_id,
        )
        return {
            "hostId": peer_host_id,
            "revision": revision,
            "data": [grant.public_dict() for grant in grants],
        }

    def suspend(self, params: Mapping[str, Any]) -> dict[str, object]:
        return self._lifecycle(params, action="suspend")

    def revoke(self, params: Mapping[str, Any]) -> dict[str, object]:
        return self._lifecycle(params, action="revoke")

    def remove(self, params: Mapping[str, Any]) -> dict[str, object]:
        return self._lifecycle(params, action="remove")

    def rotate(self, params: Mapping[str, Any]) -> dict[str, object]:
        return self._lifecycle(params, action="rotate")

    def peer_read(self, method: str, params: Mapping[str, Any]) -> dict[str, Any]:
        """Route only Stage 4 read methods to a selected paired peer."""

        if method not in {"workspace/list", "session/list", "session/search"}:
            raise BrokerError.not_found()
        if not isinstance(params, Mapping) or "hostId" not in params:
            raise BrokerError.invalid_request()
        peer_host_id = _identifier(params["hostId"])
        peer_params = dict(params)
        peer_params.pop("hostId")
        if peer_host_id == self.host_id:
            raise BrokerError.conflict()
        if self.config.role is not Role.COORDINATOR:
            raise BrokerError.unauthorized()
        relationship = self._paired_relationship(peer_host_id)
        response = self._client.operation(
            _peer_from_relationship(relationship),
            method,
            peer_params,
            timeout_seconds=PEER_IO_TIMEOUT_SECONDS,
        )
        _assert_peer_projection(response)
        return dict(response)

    def peer_session(
        self, method: str, peer_host_id: str, params: Mapping[str, Any]
    ) -> dict[str, Any]:
        """Route one fixed Stage 5 operation without forwarding host selectors."""

        if method not in SESSION_OPERATIONS:
            raise BrokerError.not_found()
        peer_host_id = _identifier(peer_host_id)
        peer_params = validate_session_params(method, params)
        if self.config.role is not Role.COORDINATOR:
            raise BrokerError.unauthorized()
        relationship = self._paired_relationship(peer_host_id)
        timeout_seconds = PEER_IO_TIMEOUT_SECONDS
        if method == "session/wait":
            timeout_seconds = float(peer_params["timeoutSeconds"]) + 5.0
        response = self._client.operation(
            _peer_from_relationship(relationship),
            method,
            peer_params,
            timeout_seconds=timeout_seconds,
        )
        return validate_session_result(
            method,
            response,
            self.config.limits.max_result_bytes,
        )

    def peer_message(
        self, source: MessageSource, submission: MessageSubmission
    ) -> dict[str, Any]:
        """Route a message only after fixed peer capability negotiation."""

        if (
            source.host_id != self.host_id
            or submission.target.host_id == self.host_id
            or submission.message_id is None
            or submission.correlation_id is None
        ):
            raise BrokerError.invalid_request()
        if self.config.role is not Role.COORDINATOR:
            raise BrokerError.unauthorized()
        relationship = self._paired_relationship(submission.target.host_id)
        peer = _peer_from_relationship(relationship)
        self._negotiate_message(peer, relationship)
        response = self._client.operation(
            peer,
            MESSAGE_OPERATION,
            peer_message_params(source, submission),
            timeout_seconds=PEER_IO_TIMEOUT_SECONDS,
        )
        return validate_message_receipt(
            response, self.config.limits.max_result_bytes
        )

    def handle_peer_request(
        self, frame: Mapping[str, Any], certificate: CertificateMaterial
    ) -> dict[str, Any]:
        """Verify a TLS-pinned signed frame before dispatching a peer request."""

        try:
            request = _parse_request(frame, certificate, self.state)
        except BrokerError as error:
            try:
                self.state.record_audit(
                    actor_host_id=certificate.host_id,
                    peer_host_id=certificate.host_id,
                    action="peer.invalid",
                    scope=None,
                    result="denied",
                    reason=error.code.value,
                )
            except BrokerError:
                pass
            return self.error_response(
                request_id=_request_id_or_empty(
                    frame.get("requestId") if isinstance(frame, Mapping) else None
                ),
                error=error,
            )
        source = request["from"]
        source_host_id = source["hostId"]
        try:
            if not self._certificate_allowed(certificate):
                raise BrokerError.unauthorized()
            kind = request["kind"]
            admission: ReadAdmission | None = None
            if kind == "operation":
                result, admission = self._handle_operation(request, certificate)
            elif kind == "pairing":
                result = self._handle_pairing(request, certificate)
            elif kind == "grant":
                result = self._handle_grant(request, certificate)
            elif kind == "lifecycle":
                result = self._handle_lifecycle(request, certificate)
            else:
                raise BrokerError.invalid_request()
            if admission is not None:
                return self.state.finalize_admission(
                    admission,
                    action=f"peer.{kind}",
                    response_factory=lambda: _signed_response(
                        self.identity,
                        self.config.role,
                        request_id=request["requestId"],
                        result={"status": "ok", "data": result},
                    ),
                )
            self.state.record_audit(
                actor_host_id=source_host_id,
                peer_host_id=source_host_id,
                action=f"peer.{kind}",
                scope=_request_scope(request),
                result="ok",
                reason=None,
            )
            return _signed_response(
                self.identity,
                self.config.role,
                request_id=request["requestId"],
                result={"status": "ok", "data": result},
            )
        except BrokerError as error:
            self._record_denial(source_host_id, request, error)
            return self.error_response(request_id=request["requestId"], error=error)

    def error_response(self, *, request_id: str, error: BrokerError) -> dict[str, Any]:
        return _signed_response(
            self.identity,
            self.config.role,
            request_id=request_id or "req_error",
            result={
                "status": "error",
                "error": {
                    "code": error.code.value,
                    "retryable": error.retryable,
                },
            },
        )

    def _handle_operation(
        self, request: Mapping[str, Any], certificate: CertificateMaterial
    ) -> tuple[dict[str, Any], ReadAdmission | None]:
        operation = request["operation"]
        params = request["params"]
        source = request["from"]
        source_host_id = source["hostId"]
        source_role = Role(source["role"])
        if operation == "host/describe":
            _exact_fields(params, set())
            return self._describe(), None
        if operation == MESSAGE_OPERATION:
            messages = self._message_service
            if messages is None:
                raise BrokerError.unavailable()
            message_source, message = validate_peer_message(
                params, self.config.limits.max_message_bytes
            )
            if message_source.host_id != source_host_id:
                raise BrokerError.unauthorized()
            scope = (
                "cancellation"
                if message.delivery == DELIVERY_INTERRUPT
                else "sessionWrite"
            )
            admission = self.state.admit_write(
                peer_host_id=source_host_id,
                peer_role=source_role,
                certificate_fingerprint=certificate.fingerprint,
                scope=scope,
            )
            return (
                messages.receive_peer(
                    source_host_id,
                    params,
                    admission.policy,
                    lambda: self.state.validate_admission(admission),
                ),
                admission,
            )
        if operation in SESSION_OPERATIONS:
            sessions = self._session_operations
            if sessions is None:
                raise BrokerError.unavailable()
            peer_params = validate_session_params(operation, params)
            scope = "cancellation" if operation == "session/cancel" else "sessionWrite"
            admission = self.state.admit_write(
                peer_host_id=source_host_id,
                peer_role=source_role,
                certificate_fingerprint=certificate.fingerprint,
                scope=scope,
            )
            return (
                sessions.handle(
                    source_host_id,
                    operation,
                    peer_params,
                    admission.policy,
                    lambda: self.state.validate_admission(admission),
                ),
                admission,
            )
        if operation not in {"workspace/list", "session/list", "session/search"}:
            raise BrokerError.not_found()
        scope = "workspaceRead" if operation == "workspace/list" else "sessionRead"
        admission = self.state.admit_read(
            peer_host_id=source_host_id,
            peer_role=source_role,
            certificate_fingerprint=certificate.fingerprint,
            scope=scope,
        )
        if operation == "workspace/list":
            _exact_fields(params, set())
            return self._peer_workspace_list(admission.policy), admission
        if operation == "session/list":
            _exact_fields(params, {"cursor", "limit"})
            return self._peer_sessions_list(admission.policy, params), admission
        _exact_fields(params, {"query", "cursor", "limit"}, {"query"})
        return self._peer_sessions_search(admission.policy, params), admission

    def _handle_pairing(
        self, request: Mapping[str, Any], certificate: CertificateMaterial
    ) -> dict[str, Any]:
        payload = _payload(request, {"action", "targetHostId", "targetRole", "sourceEndpoint"})
        if payload["action"] != "pair" or payload["targetHostId"] != self.host_id:
            raise BrokerError.invalid_request()
        source = request["from"]
        source_host_id = source["hostId"]
        try:
            source_role = Role(source["role"])
            target_role = Role(payload["targetRole"])
        except (TypeError, ValueError) as error:
            raise BrokerError.invalid_request() from error
        if source_role is not Role.COORDINATOR or target_role is not self.config.role:
            raise BrokerError.unauthorized()
        source_endpoint = PeerEndpoint.parse(payload["sourceEndpoint"]).value
        if self.config.role is Role.MANAGED:
            if certificate.fingerprint not in self._eligible_certificates:
                raise BrokerError.unauthorized()
            relationship = self.state.accept_pair(
                peer_host_id=source_host_id,
                peer_role=source_role,
                certificate=certificate,
                endpoint=source_endpoint,
                require_pending=False,
            )
        elif self.config.role is Role.COORDINATOR:
            relationship = self.state.accept_pair(
                peer_host_id=source_host_id,
                peer_role=source_role,
                certificate=certificate,
                endpoint=source_endpoint,
                require_pending=True,
            )
        else:
            raise BrokerError.unauthorized()
        return {"status": "paired", "relationship": relationship.public_dict()}

    def _handle_grant(
        self, request: Mapping[str, Any], certificate: CertificateMaterial
    ) -> dict[str, Any]:
        payload = _payload(request, {"action", "targetHostId", "revision", "grants"})
        if payload["action"] != "replace" or payload["targetHostId"] != self.host_id:
            raise BrokerError.invalid_request()
        source = request["from"]
        source_host_id = source["hostId"]
        if Role(source["role"]) is not Role.COORDINATOR:
            raise BrokerError.unauthorized()
        self._authorize_lifecycle_source(source_host_id, certificate)
        revision = payload["revision"]
        if not isinstance(revision, int) or isinstance(revision, bool):
            raise BrokerError.invalid_request()
        grants = _grants_from_payload(payload["grants"])
        self.state.replace_grants(
            peer_host_id=source_host_id,
            revision=revision,
            grants=grants,
            actor_host_id=source_host_id,
        )
        return {"revision": revision}

    def _handle_lifecycle(
        self, request: Mapping[str, Any], certificate: CertificateMaterial
    ) -> dict[str, Any]:
        payload = _payload(request, {"action", "targetHostId"})
        if payload["targetHostId"] != self.host_id:
            raise BrokerError.invalid_request()
        source = request["from"]
        source_host_id = source["hostId"]
        if Role(source["role"]) is not Role.COORDINATOR:
            raise BrokerError.unauthorized()
        self._authorize_lifecycle_source(source_host_id, certificate)
        action = payload["action"]
        if action == "suspend":
            relationship = self.state.set_status(
                peer_host_id=source_host_id,
                status="suspended",
                actor_host_id=source_host_id,
            )
            self._detach_peer(source_host_id)
            return {"status": relationship.status}
        if action in {"revoke", "rotate"}:
            relationship = self.state.set_status(
                peer_host_id=source_host_id,
                status="revoked" if action == "revoke" else "suspended",
                actor_host_id=source_host_id,
            )
            self._detach_peer(source_host_id)
            return {"status": relationship.status}
        if action == "remove":
            self.state.remove(peer_host_id=source_host_id, actor_host_id=source_host_id)
            self._detach_peer(source_host_id)
            return {"status": "removed"}
        raise BrokerError.invalid_request()

    def _describe(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "hostId": self.host_id,
            "role": self.config.role.value,
            "protocol": PROTOCOL,
            "protocolVersion": PROTOCOL_VERSION,
            "protocolMajor": PROTOCOL_MAJOR,
            "protocolMinor": PROTOCOL_MINOR,
            "fingerprint": self.identity.certificate.fingerprint,
            "capabilities": [
                "workspaceRead",
                "sessionRead",
                "sessionWrite",
                "cancellation",
                MESSAGE_CAPABILITY,
            ],
        }
        if self._server is not None:
            value["endpoint"] = self._server.endpoint
        return value

    def _peer_workspace_list(self, policy: GrantPolicy) -> dict[str, Any]:
        data = [
            item
            for item in self.catalog.workspace_list()
            if isinstance(item, Mapping)
            and isinstance(item.get("workspaceId"), str)
            and policy.permits_workspace(item["workspaceId"])
        ]
        return {"data": data}

    def _peer_sessions_list(
        self, policy: GrantPolicy, params: Mapping[str, Any]
    ) -> dict[str, Any]:
        page = self.catalog.list_sessions(
            cursor=params.get("cursor"),
            limit=params.get("limit"),
        ).to_dict()
        return _project_session_page(page, self.catalog.workspaces, policy)

    def _peer_sessions_search(
        self, policy: GrantPolicy, params: Mapping[str, Any]
    ) -> dict[str, Any]:
        page = self.catalog.search_sessions(
            params["query"],
            cursor=params.get("cursor"),
            limit=params.get("limit"),
        ).to_dict()
        return _project_search_page(page, self.catalog.workspaces, policy)

    def _find_candidate(
        self,
        host_id: str,
        role: Role,
        *,
        fingerprint: str | None,
    ) -> DiscoveredPeer:
        deadline = time.monotonic() + min(
            PEER_IO_TIMEOUT_SECONDS,
            self.config.limits.max_discovery_seconds,
        )
        for peer in self._static_peers:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            try:
                response = self._client.operation(
                    peer,
                    "host/describe",
                    {},
                    timeout_seconds=remaining,
                )
                candidate = _discovered_peer(response, peer)
            except BrokerError:
                continue
            if (
                candidate.host_id == host_id
                and candidate.role is role
                and (fingerprint is None or candidate.fingerprint == fingerprint)
            ):
                return candidate
        raise BrokerError.not_found()

    def _certificate_for_candidate(self, candidate: DiscoveredPeer) -> CertificateMaterial:
        for peer in self._static_peers:
            if (
                peer.endpoint.value == candidate.endpoint
                and peer.certificate.fingerprint == candidate.fingerprint
            ):
                return peer.certificate
        raise BrokerError.not_found()

    def _peer_for_candidate(self, candidate: DiscoveredPeer) -> StaticPeer:
        return StaticPeer(
            endpoint=PeerEndpoint.parse(candidate.endpoint),
            certificate=self._certificate_for_candidate(candidate),
            role=candidate.role,
        )

    def _paired_relationship(self, peer_host_id: str) -> Relationship:
        relationship = self.state.relationship(peer_host_id)
        if relationship is None:
            raise BrokerError.not_found()
        if relationship.status != "paired":
            raise BrokerError.conflict()
        return relationship

    def _listener_endpoint(self) -> str:
        if self._server is None:
            raise BrokerError.unavailable()
        return self._server.endpoint

    def _certificate_allowed(self, certificate: CertificateMaterial) -> bool:
        if self.state.is_credential_tombstoned(certificate):
            return False
        if certificate.fingerprint in self._eligible_certificates:
            return True
        if any(
            peer.certificate.fingerprint == certificate.fingerprint
            for peer in self._static_peers
        ):
            return True
        return any(
            relationship.certificate.fingerprint == certificate.fingerprint
            for relationship in self.state.relationships()
            if relationship.status in {"pending", "paired", "suspended"}
        )

    def _authorize_lifecycle_source(
        self, source_host_id: str, certificate: CertificateMaterial
    ) -> None:
        relationship = self.state.relationship(source_host_id)
        if (
            relationship is None
            or relationship.status not in {"pending", "paired", "suspended", "revoked"}
            or relationship.peer_role is not Role.COORDINATOR
            or not secrets.compare_digest(
                relationship.certificate.fingerprint, certificate.fingerprint
            )
        ):
            raise BrokerError.unauthorized()

    def _lifecycle(self, params: Mapping[str, Any], *, action: str) -> dict[str, object]:
        _exact_fields(params, {"hostId"}, {"hostId"})
        peer_host_id = _identifier(params["hostId"])
        relationship = self.state.relationship(peer_host_id)
        if relationship is None:
            raise BrokerError.not_found()
        if action == "remove":
            self.state.remove(peer_host_id=peer_host_id, actor_host_id=self.host_id)
        else:
            status = "revoked" if action == "revoke" else "suspended"
            self.state.set_status(
                peer_host_id=peer_host_id,
                status=status,
                actor_host_id=self.host_id,
            )
        self._detach_peer(peer_host_id)
        propagated = False
        try:
            self._client.control(
                _peer_from_relationship(relationship),
                "lifecycle",
                {"action": action, "targetHostId": peer_host_id},
                timeout_seconds=PEER_IO_TIMEOUT_SECONDS,
            )
            propagated = True
        except BrokerError:
            pass
        return {
            "hostId": peer_host_id,
            "status": "removed" if action == "remove" else status,
            "propagated": propagated,
        }

    def _detach_peer(self, peer_host_id: str) -> None:
        sessions = self._session_operations
        if sessions is not None:
            sessions.detach_peer(peer_host_id)

    def _negotiate_message(
        self, peer: StaticPeer, relationship: Relationship
    ) -> None:
        response = self._client.operation(
            peer,
            "host/describe",
            {},
            timeout_seconds=PEER_IO_TIMEOUT_SECONDS,
        )
        candidate = _discovered_peer(response, peer)
        if (
            candidate.host_id != relationship.peer_host_id
            or candidate.role is not relationship.peer_role
            or candidate.protocol_major != PROTOCOL_MAJOR
        ):
            raise BrokerError.unauthorized()
        if (
            candidate.protocol_minor < PROTOCOL_MINOR
            or MESSAGE_CAPABILITY not in candidate.capabilities
        ):
            raise BrokerError.unavailable()

    def _record_denial(
        self, source_host_id: str, request: Mapping[str, Any], error: BrokerError
    ) -> None:
        try:
            self.state.record_audit(
                actor_host_id=source_host_id,
                peer_host_id=source_host_id,
                action=f"peer.{request.get('kind', 'invalid')}",
                scope=_request_scope(request),
                result="denied",
                reason=error.code.value,
            )
        except BrokerError:
            return

    def _require_coordinator(self) -> None:
        if self.config.role is not Role.COORDINATOR:
            raise BrokerError.unauthorized()

    def _record_local_review(self, action: str, result: str) -> None:
        self.state.record_audit(
            actor_host_id=self.host_id,
            peer_host_id=None,
            action=action,
            scope=None,
            result=result,
            reason=None,
        )


def _static_peers(values: tuple[StaticPeerConfig, ...]) -> tuple[StaticPeer, ...]:
    peers: list[StaticPeer] = []
    seen: set[tuple[str, str]] = set()
    for value in values:
        peer = StaticPeer(
            endpoint=PeerEndpoint.parse(value.endpoint),
            certificate=load_certificate(value.certificate_path),
        )
        key = (peer.endpoint.value, peer.certificate.fingerprint)
        if key in seen:
            raise BrokerError.invalid_request()
        seen.add(key)
        peers.append(peer)
    return tuple(peers)


def _optional_review_reason(value: Any) -> None:
    if value is not None and (
        not isinstance(value, str) or len(value.encode("utf-8")) > 1_024
    ):
        raise BrokerError.invalid_request()


def _project_session_page(
    page: Mapping[str, Any],
    workspaces: Any,
    policy: GrantPolicy,
) -> dict[str, Any]:
    data = page.get("data")
    if not isinstance(data, list):
        raise BrokerError.internal()
    projected: list[dict[str, Any]] = []
    for item in data:
        projection = _project_session(item, workspaces, policy)
        if projection is not None:
            projected.append(projection)
    return {"data": projected, "nextCursor": page.get("nextCursor")}


def _project_search_page(
    page: Mapping[str, Any],
    workspaces: Any,
    policy: GrantPolicy,
) -> dict[str, Any]:
    data = page.get("data")
    if not isinstance(data, list):
        raise BrokerError.internal()
    projected: list[dict[str, Any]] = []
    for item in data:
        if not isinstance(item, Mapping):
            continue
        thread = _project_session(item.get("thread"), workspaces, policy)
        if thread is None:
            continue
        snippet = item.get("snippet", "")
        if not isinstance(snippet, str):
            snippet = ""
        projected.append({"thread": thread, "snippet": snippet})
    return {"data": projected, "nextCursor": page.get("nextCursor")}


def _project_session(
    value: Any,
    workspaces: Any,
    policy: GrantPolicy,
) -> dict[str, Any] | None:
    if not isinstance(value, Mapping):
        return None
    thread_id = value.get("threadId")
    cwd = value.get("cwd")
    if not isinstance(thread_id, str) or not isinstance(cwd, str):
        return None
    workspace = workspaces.workspace_for_path(cwd)
    if workspace is None or not policy.permits_session(workspace.workspace_id, thread_id):
        return None
    return {
        key: item
        for key, item in value.items()
        if key in {"hostId", "threadId", "isRunning", "lastActivity", "summary"}
    }


def _assert_peer_projection(value: Mapping[str, Any]) -> None:
    def visit(item: Any) -> None:
        if isinstance(item, Mapping):
            for key, child in item.items():
                if key in {"cwd", "path", "root", "certificate", "certificatePath"}:
                    raise BrokerError.internal()
                visit(child)
        elif isinstance(item, list):
            for child in item:
                visit(child)

    visit(value)


def _discovered_peer(response: Mapping[str, Any], peer: StaticPeer) -> DiscoveredPeer:
    required = {
        "hostId",
        "role",
        "protocol",
        "protocolVersion",
        "fingerprint",
        "capabilities",
    }
    allowed = required | {"endpoint", "protocolMajor", "protocolMinor"}
    if not required.issubset(response) or set(response) - allowed:
        raise BrokerError.invalid_request()
    protocol_major = response.get("protocolMajor", response["protocolVersion"])
    protocol_minor = response.get("protocolMinor", 0)
    if (
        response["protocol"] != PROTOCOL
        or response["protocolVersion"] != protocol_major
        or not isinstance(protocol_major, int)
        or isinstance(protocol_major, bool)
        or protocol_major != PROTOCOL_MAJOR
        or not isinstance(protocol_minor, int)
        or isinstance(protocol_minor, bool)
        or protocol_minor < 0
    ):
        raise BrokerError.invalid_request()
    host_id = _identifier(response["hostId"])
    try:
        role = Role(response["role"])
    except (TypeError, ValueError) as error:
        raise BrokerError.invalid_request() from error
    if host_id != peer.certificate.host_id:
        raise BrokerError.unauthorized()
    fingerprint = response["fingerprint"]
    if (
        not _fingerprint(fingerprint)
        or not secrets.compare_digest(fingerprint, peer.certificate.fingerprint)
    ):
        raise BrokerError.unauthorized()
    endpoint = response.get("endpoint", peer.endpoint.value)
    if not isinstance(endpoint, str):
        raise BrokerError.invalid_request()
    endpoint = PeerEndpoint.parse(endpoint).value
    capabilities = response["capabilities"]
    if (
        not isinstance(capabilities, list)
        or len(capabilities) > 32
        or any(not isinstance(capability, str) for capability in capabilities)
    ):
        raise BrokerError.invalid_request()
    capability_names = frozenset(capabilities)
    if any(
        not capability
        or len(capability) > 64
        or any(character.isspace() or ord(character) < 33 for character in capability)
        for capability in capability_names
    ):
        raise BrokerError.invalid_request()
    return DiscoveredPeer(
        host_id=host_id,
        role=role,
        endpoint=endpoint,
        fingerprint=fingerprint,
        protocol_major=protocol_major,
        protocol_minor=protocol_minor,
        capabilities=capability_names,
    )


def _peer_from_relationship(relationship: Relationship) -> StaticPeer:
    return StaticPeer(
        endpoint=PeerEndpoint.parse(relationship.endpoint),
        certificate=relationship.certificate,
        role=relationship.peer_role,
    )


def _request_scope(request: Mapping[str, Any]) -> str | None:
    operation = request.get("operation")
    if operation == "workspace/list":
        return "workspaceRead"
    if operation in {"session/list", "session/search"}:
        return "sessionRead"
    if operation == "session/cancel":
        return "cancellation"
    if operation == MESSAGE_OPERATION:
        params = request.get("params")
        if isinstance(params, Mapping) and params.get("delivery") == DELIVERY_INTERRUPT:
            return "cancellation"
        return "sessionWrite"
    if operation in SESSION_OPERATIONS:
        return "sessionWrite"
    return None
