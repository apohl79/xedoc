#!/usr/bin/env python3
"""Exercise packaged hostname recovery and multi-address TLS with real sockets."""

import argparse
from dataclasses import replace
from pathlib import Path
import socket
import sys
import tempfile
import time
from types import SimpleNamespace

from xedoc_package.remote_agent_payload import build_payload_archive


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="xedoc-dns-recovery-") as directory:
        root = Path(directory).resolve()
        payload = args.payload or root / "remote-agent.pyz"
        if args.payload is None:
            build_payload_archive(payload, force=False)
        sys.path.insert(0, str(payload))

        from xedoc_remote_agent import peer
        from xedoc_remote_agent.errors import BrokerError
        from xedoc_remote_agent.models import BrokerConfig, BrokerLimits, Role
        from xedoc_remote_agent.peer_protocol import PeerEndpoint, PeerTarget
        from xedoc_remote_agent.peer_state import Grant, PeerState
        from xedoc_remote_agent.peer_transport import PeerServer

        limits = BrokerLimits(8, 65536, 65536, 30, 15, 7)
        expected = {"data": [{"workspaceId": "dns-workspace"}]}
        catalog = SimpleNamespace(workspace_list=lambda: expected["data"])
        hostname = "paired-recovery.test"
        original_hostname = peer._hostname
        original_getaddrinfo = socket.getaddrinfo
        answers = ["::1", "127.0.0.1"]
        servers = []

        def resolve(host, port, *args, **kwargs):
            if host != hostname:
                return original_getaddrinfo(host, port, *args, **kwargs)
            results = []
            for address in answers:
                results.extend(original_getaddrinfo(address, port, *args, **kwargs))
            return results

        def host(name, role):
            state = PeerState(root / name, audit_retention_days=7)
            return peer.PeerService(
                identity=state.ensure_identity(),
                state=state,
                config=BrokerConfig(role, (), limits),
                catalog=catalog,
            )

        def serve(service, address, port=0, *, bootstrap=False):
            server = PeerServer(
                service,
                PeerEndpoint(address, port),
                max_request_bytes=65536,
                max_response_bytes=65536,
                bootstrap=bootstrap,
            )
            server.start()
            servers.append(server)
            if not bootstrap:
                service._server = server
            return server

        coordinator = host("coordinator", Role.COORDINATOR)
        managed = host("managed", Role.MANAGED)
        rogue = host("rogue", Role.MANAGED)
        try:
            peer._hostname = lambda: hostname
            socket.getaddrinfo = resolve
            target = serve(managed, "::1")
            old_endpoint = PeerEndpoint.parse(target.endpoint)
            for source, destination, endpoint in (
                (coordinator, managed, old_endpoint.value),
                (managed, coordinator, "tls://127.0.0.1:1"),
            ):
                source.state.accept_pair(
                    peer_host_id=destination.host_id,
                    peer_role=destination.config.role,
                    certificate=destination.identity.certificate,
                    endpoint=endpoint,
                    require_pending=False,
                )
            managed.state.replace_grants(
                peer_host_id=coordinator.host_id,
                revision=1,
                grants=(Grant("workspaceRead", None, None, None),),
                actor_host_id=coordinator.host_id,
            )

            def workspaces(service):
                return service.peer_read("workspace/list", {"hostId": managed.host_id})

            def relationship(service):
                return service.state.relationship(managed.host_id)

            def unchanged_trust(before, after):
                assert (
                    replace(
                        after, endpoint=before.endpoint, updated_at=before.updated_at
                    )
                    == before
                )

            assert workspaces(coordinator) == expected
            coordinator._refresh_hosts()
            before = relationship(coordinator)
            grants_before = managed.state.current_grants(coordinator.host_id)
            reloaded = host("coordinator", Role.COORDINATOR)
            assert reloaded.hosts_list({})["data"][0]["hostname"] == hostname
            assert reloaded.resolve_host(hostname) == managed.host_id
            assert reloaded._discovery is None
            print("PASS: authenticated hostname survives reload without discovery")

            target.stop()
            serve(rogue, "::1", old_endpoint.port)
            serve(managed, "127.0.0.1", old_endpoint.port)
            reloaded._refresh_hosts()
            recovered_endpoint = PeerEndpoint(hostname, old_endpoint.port).value
            assert relationship(reloaded).endpoint == recovered_endpoint
            assert reloaded.hosts_list({})["data"][0]["reachable"] is True
            assert workspaces(reloaded) == expected
            unchanged_trust(before, relationship(reloaded))
            assert managed.state.current_grants(coordinator.host_id) == grants_before
            reloaded = host("coordinator", Role.COORDINATOR)
            assert workspaces(reloaded) == expected
            assert relationship(reloaded).certificate == managed.identity.certificate
            print(
                "PASS: stale numeric endpoint recovers through saved DNS; wrong IPv6 "
                "certificate falls back to pinned IPv4 host with unchanged trust/grants"
            )

            answers[:] = ["::1"]
            before = relationship(reloaded)
            peer._hostname = lambda: "attacker-name.test"
            reloaded._refresh_hosts()
            assert relationship(reloaded) == before
            assert reloaded.hosts_list({})["data"][0]["reachable"] is False
            assert reloaded.hosts_list({})["data"][0]["hostname"] == hostname
            try:
                workspaces(reloaded)
            except BrokerError:
                pass
            else:
                raise AssertionError(
                    "All-wrong DNS unexpectedly authorized workspace/list"
                )
            assert managed.state.current_grants(coordinator.host_id) == grants_before
            print(
                "PASS: all-wrong DNS cannot rename, redirect, or authorize a paired host"
            )

            answers[:] = ["::1", "127.0.0.1"]
            peer._hostname = lambda: hostname
            target_peer = PeerTarget(
                PeerEndpoint(hostname, old_endpoint.port),
                managed.identity.certificate,
                Role.MANAGED,
            )
            assert (
                coordinator._client.operation(
                    target_peer, "workspace/list", {}, timeout_seconds=3
                )
                == expected
            )

            with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as stalled:
                stalled.bind(("::1", 0))
                stalled.listen(1)
                port = stalled.getsockname()[1]
                serve(managed, "127.0.0.1", port)
                started = time.monotonic()
                assert (
                    coordinator._client.operation(
                        replace(target_peer, endpoint=PeerEndpoint(hostname, port)),
                        "workspace/list",
                        {},
                        timeout_seconds=1,
                    )
                    == expected
                )
                assert time.monotonic() - started < 2
            print("PASS: stalled IPv6 TLS leaves time for pinned IPv4 fallback")

            first = serve(managed, "::1")
            port = PeerEndpoint.parse(first.endpoint).port
            serve(managed, "127.0.0.1", port)
            handle_request = managed.handle_peer_request
            received = []

            def fail_after_request(request, certificate):
                received.append(request["requestId"])
                handle_request(request, certificate)
                raise OSError("response lost after accepting request")

            managed.handle_peer_request = fail_after_request
            try:
                try:
                    coordinator._client.operation(
                        replace(target_peer, endpoint=PeerEndpoint(hostname, port)),
                        "workspace/list",
                        {},
                        timeout_seconds=3,
                    )
                except BrokerError:
                    pass
                else:
                    raise AssertionError("Lost response unexpectedly succeeded")
                assert len(received) == 1, "Request was replayed on another address"
            finally:
                managed.handle_peer_request = handle_request
            print("PASS: post-request failure never replays on another DNS address")

            bootstrap_host = host("bootstrap-target", Role.COORDINATOR)
            pairing = serve(bootstrap_host, "127.0.0.1", bootstrap=True)
            pairing_port = PeerEndpoint.parse(pairing.endpoint).port
            serve(rogue, "::1", pairing_port, bootstrap=True)
            response, certificate = coordinator._client.bootstrap_pairing(
                PeerEndpoint(hostname, pairing_port),
                host_id=bootstrap_host.host_id,
                fingerprint=bootstrap_host.identity.certificate.fingerprint,
                role=Role.COORDINATOR,
                payload={
                    "action": "pair",
                    "targetHostId": bootstrap_host.host_id,
                    "targetRole": "coordinator",
                    "sourceEndpoint": "tls://127.0.0.1:1",
                    "pairingId": "pair_dns_acceptance_123456789",
                    "expiresAt": int(time.time()) + 60,
                    "sourceCertificate": coordinator.identity.certificate.pem.decode(
                        "ascii"
                    ),
                },
                timeout_seconds=3,
            )
            assert certificate == bootstrap_host.identity.certificate
            assert response["status"] == "pending"
            assert (
                bootstrap_host.state.relationship(coordinator.host_id).certificate
                == coordinator.identity.certificate
            )
            assert rogue.state.relationship(coordinator.host_id) is None
            assert relationship(reloaded) == before
            print(
                "PASS: bootstrap pairing skips wrong IPv6 pin and reaches valid IPv4 pin"
            )
        finally:
            socket.getaddrinfo = original_getaddrinfo
            peer._hostname = original_hostname
            for server in reversed(servers):
                server.stop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
