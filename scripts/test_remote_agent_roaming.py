#!/usr/bin/env python3
"""Exercise packaged peer relocation and discovery recovery with real sockets."""

import argparse
from dataclasses import replace
import json
from pathlib import Path
import socket
import sys
import tempfile
import time
from types import SimpleNamespace

from xedoc_package.remote_agent_payload import build_payload_archive


def wait_for(predicate, description):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(f"Timed out: {description}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--payload", type=Path)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix="xedoc-roaming-") as directory:
        root = Path(directory).resolve()
        payload = args.payload or root / "remote-agent.pyz"
        if args.payload is None:
            build_payload_archive(payload, force=False)
        sys.path.insert(0, str(payload))

        from xedoc_remote_agent import peer_discovery as discovery
        from xedoc_remote_agent.models import BrokerConfig, BrokerLimits, Role
        from xedoc_remote_agent.peer import PeerService
        from xedoc_remote_agent.peer_protocol import PeerEndpoint
        from xedoc_remote_agent.peer_state import Grant, PeerState
        from xedoc_remote_agent.peer_transport import PeerServer

        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
            reservation.bind(("", 0))
            port = reservation.getsockname()[1]
        discovery.DISCOVERY_PORT = port
        discovery.DISCOVERY_PORT_SLOTS = 1
        discovery.DISCOVERY_GROUP = "239.255.70.41"
        discovery._LOCAL_DISCOVERY_DIRECTORY = root / "discovery"

        limits = BrokerLimits(8, 65536, 65536, 30, 15, 7)
        catalog = SimpleNamespace(workspace_list=lambda: [{"workspaceId": "workspace"}])

        def host(name, role):
            state = PeerState(root / name, audit_retention_days=7)
            return PeerService(
                identity=state.ensure_identity(),
                state=state,
                config=BrokerConfig(role, (), limits),
                catalog=catalog,
            )

        coordinator = host("coordinator", Role.COORDINATOR)
        managed = host("managed", Role.MANAGED)
        rogue = host("rogue", Role.MANAGED)

        def serve(service):
            server = PeerServer(
                service,
                PeerEndpoint("0.0.0.0", 0),
                max_request_bytes=65536,
                max_response_bytes=65536,
            )
            server.start()
            service._server = server
            return server

        target = serve(managed)
        impostor = serve(rogue)
        old_endpoint = PeerEndpoint(
            "127.0.0.1", PeerEndpoint.parse(target.endpoint).port
        )
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
        expected = {"data": [{"workspaceId": "workspace"}]}

        def workspaces(service=coordinator):
            return service.peer_read("workspace/list", {"hostId": managed.host_id})

        def relationship():
            return coordinator.state.relationship(managed.host_id)

        receiver = discovery.PeerDiscovery(
            host_id=coordinator.host_id,
            hostname="roaming-coordinator",
            role=Role.COORDINATOR,
            fingerprint_value=coordinator.identity.certificate.fingerprint,
            endpoints=lambda address: (old_endpoint, old_endpoint),
            capabilities=frozenset,
            on_announce=coordinator._on_announce,
        )

        def announce(endpoint, **overrides):
            packet = {
                "protocol": discovery.DISCOVERY_PROTOCOL,
                "version": discovery.DISCOVERY_VERSION,
                "kind": "announce",
                "hostId": managed.host_id,
                "hostname": "forged-name",
                "role": "managed",
                "endpoint": endpoint.value,
                "pairingEndpoint": endpoint.value,
                "fingerprint": managed.identity.certificate.fingerprint,
                "protocolMajor": 1,
                "protocolMinor": 1,
                "capabilities": [],
                "ttlSeconds": 95,
                **overrides,
            }
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sender:
                sender.bind((endpoint.host, 0))
                sender.sendto(json.dumps(packet).encode(), ("127.0.0.1", port))

        try:
            assert workspaces() == expected
            bind_listener = discovery._bind_listener

            def offline():
                raise OSError("simulated offline interface")

            discovery._bind_listener = offline
            try:
                receiver.start()
                try:
                    receiver.start()
                except discovery.BrokerError:
                    pass
                else:
                    raise AssertionError("Duplicate discovery start was accepted")
            finally:
                discovery._bind_listener = bind_listener
            wait_for(lambda: receiver._socket is not None, "offline startup recovery")
            print(
                "PASS: discovery starts offline and recovers without duplicate workers"
            )
            coordinator._start_host_refresh()
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
                probe.connect((discovery.DISCOVERY_GROUP, 1))
                lan_address = probe.getsockname()[0]
            assert lan_address != "0.0.0.0", (
                "Address-change scenario requires a LAN IPv4 address"
            )
            relocated = PeerEndpoint(lan_address, old_endpoint.port)
            before = relationship()
            announce(relocated)
            wait_for(
                lambda: relationship().endpoint == relocated.value, "address relocation"
            )
            assert workspaces() == expected
            after = relationship()
            assert (
                replace(after, endpoint=before.endpoint, updated_at=before.updated_at)
                == before
            )
            print("PASS: wildcard listener survives an authenticated address change")

            target.stop()
            target = serve(managed)
            restarted = PeerEndpoint(
                "127.0.0.1", PeerEndpoint.parse(target.endpoint).port
            )
            announce(restarted)
            wait_for(
                lambda: relationship().endpoint == restarted.value, "port relocation"
            )
            assert workspaces() == expected
            reloaded = host("coordinator", Role.COORDINATOR)
            assert workspaces(reloaded) == expected
            print(
                "PASS: changed port persists across coordinator reload; workspace/list succeeds"
            )

            forged = PeerEndpoint(
                "127.0.0.1", PeerEndpoint.parse(impostor.endpoint).port
            )
            announce(forged)
            time.sleep(4)
            assert relationship().endpoint == restarted.value
            assert workspaces() == expected
            assert coordinator.hosts_list({})["data"][0]["hostname"] != "forged-name"
            print("PASS: a spoofed relocation cannot replace the pinned host")

            listener = receiver._socket
            discovery.MEMBERSHIP_REFRESH_INTERVAL_SECONDS = 0.2
            discovery.NETWORK_CHECK_INTERVAL_SECONDS = 0.1
            discovery.ipv4_addresses = lambda: ("192.0.2.1",)
            wait_for(
                lambda: receiver._socket is not listener, "network listener refresh"
            )
            listener = receiver._socket
            listener.close()
            wait_for(
                lambda: (
                    receiver._socket is not None and receiver._socket is not listener
                ),
                "closed listener recovery",
            )
            relocated = PeerEndpoint(lan_address, restarted.port)
            announce(relocated)
            wait_for(
                lambda: relationship().endpoint == relocated.value,
                "post-rebind announcement",
            )
            assert workspaces() == expected
            print(
                "PASS: network refresh and closed-socket recovery retain discovery and TLS"
            )

            operation = coordinator._client.operation

            def revoke_during_probe(peer, *args, **kwargs):
                result = operation(peer, *args, **kwargs)
                if peer.endpoint == restarted:
                    coordinator.state.set_status(
                        peer_host_id=managed.host_id,
                        status="revoked",
                        actor_host_id=coordinator.host_id,
                    )
                return result

            coordinator._client.operation = revoke_during_probe
            announce(restarted)
            wait_for(lambda: relationship().status == "revoked", "in-flight revocation")
            time.sleep(4)
            assert relationship().status == "revoked"
            assert relationship().endpoint == relocated.value
            announce(restarted)
            time.sleep(1)
            assert relationship().status == "revoked"
            assert relationship().endpoint == relocated.value
            print(
                "PASS: in-flight relocation and later discovery cannot undo revocation"
            )
        finally:
            coordinator._stop_host_refresh()
            receiver.stop()
            target.stop()
            impostor.stop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
