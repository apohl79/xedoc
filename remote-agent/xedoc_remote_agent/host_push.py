"""Durable account-bound subscriptions and host-owned activity observation."""

from collections import OrderedDict, deque
import hashlib
import json
import logging
import os
from pathlib import Path
import re
import subprocess
import tempfile
import threading
import time
import uuid

import httpcore

from .controller import _default_client_factory
from .errors import BrokerError
from .host_apns import HostAPNs
from .host_outside_projects import OTHER_WORKSPACE, session_inventory
from .workspaces import load_bootstrap_descriptor


class HostPush:
    def __init__(self, home: Path, auth, workspaces, hostname: str, origin: str):
        self.path = home / "host-gui" / "push-devices.json"
        self.auth, self.workspaces = auth, workspaces
        self.hostname, self.origin = hostname, origin
        self.apns = HostAPNs(home)
        self.lock = threading.RLock()
        self._stop = threading.Event()
        self._wake = threading.Event()
        self._devices = {}
        self._revoked = set()
        self._configuration_warning = False
        if self.path.exists():
            if self.path.stat().st_size > 512 * 1024:
                raise ValueError("Push subscription store exceeds size limit")
            value = json.loads(self.path.read_text())
            if (
                not isinstance(value, dict)
                or not isinstance(value.get("devices"), dict)
                or len(value["devices"]) > 256
                or not isinstance(value.get("revoked"), list)
                or len(value["revoked"]) > 4096
                or any(
                    not isinstance(key, str) or not re.fullmatch(r"[a-f0-9]{64}", key)
                    for key in value["revoked"]
                )
            ):
                raise ValueError("Invalid push subscription store")
            for secret_hash, device in value["devices"].items():
                if not re.fullmatch(r"[a-f0-9]{64}", secret_hash):
                    raise ValueError("Invalid push subscription identity")
                self._validate(device)
                if (
                    not isinstance(device.get("username"), str)
                    or not 1 <= len(device["username"]) <= 64
                    or not re.fullmatch(
                        r"[a-f0-9]{64}", str(device.get("sessionHash", ""))
                    )
                ):
                    raise ValueError("Invalid push subscription account")
            self._devices = value["devices"]
            self._revoked = set(value["revoked"])
        self._thread = threading.Thread(
            target=self._run, name="xedoc-host-push", daemon=True
        )
        self._thread.start()

    @staticmethod
    def _validate(device: dict) -> None:
        if (
            not isinstance(device, dict)
            or not isinstance(device.get("installationId"), str)
            or not isinstance(device.get("deviceToken"), str)
            or not re.fullmatch(r"(?:[a-fA-F0-9]{2}){1,256}", device["deviceToken"])
            or device.get("environment") not in ("development", "production")
        ):
            raise ValueError("Invalid push registration")
        try:
            uuid.UUID(device["installationId"])
        except (ValueError, AttributeError) as error:
            raise ValueError("Invalid push installation ID") from error

    @staticmethod
    def _secret_hash(secret) -> str:
        if not isinstance(secret, str) or not re.fullmatch(r"[a-f0-9]{64}", secret):
            raise ValueError("Invalid push subscription ID")
        return hashlib.sha256(secret.encode()).hexdigest()

    def _save(self, devices: dict, revoked=None) -> None:
        revoked = self._revoked if revoked is None else revoked
        revoked = revoked | (self._devices.keys() - devices.keys())
        if len(revoked) > 4096:
            raise ValueError("Push revocation store is full")
        content = json.dumps({"devices": devices, "revoked": sorted(revoked)})
        if len(content.encode()) > 512 * 1024:
            raise ValueError("Push subscription store exceeds size limit")
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(dir=self.path.parent)
        try:
            with os.fdopen(descriptor, "w") as stream:
                stream.write(content)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        self._devices = devices
        self._revoked = revoked

    def status(self) -> dict:
        try:
            self.apns.configuration()
            self._configuration_warning = False
            return {"available": True}
        except (OSError, ValueError, TypeError, subprocess.SubprocessError):
            if not self._configuration_warning:
                logging.warning(
                    "APNs signing credentials unavailable; automatic provisioning requires signing-key access",
                    exc_info=True,
                )
                self._configuration_warning = True
            return {"available": False}

    def register(self, body: dict, token: str, account: dict, origin: str) -> None:
        self._validate(body)
        secret_hash = self._secret_hash(body.get("subscriptionId"))
        config = self.apns.configuration()
        if body["environment"] not in config["keys"]:
            raise OSError("APNs environment is not configured")
        device = {
            key: body[key] for key in ("installationId", "deviceToken", "environment")
        }
        device.update(
            username=account["username"],
            sessionHash=hashlib.sha256(token.encode()).hexdigest(),
            origin=origin,
        )
        with self.lock, self.auth.lock:
            self.auth.account(token)
            if secret_hash in self._revoked:
                raise ValueError(
                    "Push subscription has been revoked; create a new subscription ID"
                )
            previous = self._devices.get(secret_hash)
            if previous and (
                previous["username"] != device["username"]
                or previous["installationId"] != device["installationId"]
            ):
                raise PermissionError(
                    "Push subscription belongs to another installation"
                )
            devices = self._eligible()
            if secret_hash not in devices and len(devices) >= 256:
                raise ValueError("Push subscription limit reached")
            self._save({**devices, secret_hash: device})
        self._wake.set()

    def unregister(self, secret, token: str) -> None:
        secret_hash = self._secret_hash(secret)
        # The same lock spans submission, so acknowledgment is a send barrier.
        with self.lock:
            if secret_hash not in self._revoked:
                if secret_hash not in self._devices:
                    try:
                        self.auth.account(token)
                    except PermissionError as error:
                        raise BrokerError.unauthorized(
                            "Login required to revoke an unknown subscription"
                        ) from error
                if len(self._revoked) >= 4096:
                    raise ValueError("Push revocation store is full")
                self._save(
                    {
                        key: value
                        for key, value in self._devices.items()
                        if key != secret_hash
                    },
                    self._revoked | {secret_hash},
                )
        self._wake.set()

    def _eligible(self) -> dict:
        with self.auth.lock:
            now = time.time()
            return {
                key: value
                for key, value in self._devices.items()
                if self.auth.sessions.get(value["sessionHash"], ("", 0))[0]
                == value["username"]
                and self.auth.sessions[value["sessionHash"]][1] > now
                and value["username"] in self.auth.users
            }

    def _deliver(self, thread_id: str, kind: str) -> None:
        roots, _ = self.workspaces._config()
        with self.lock:
            recipients = list(self._eligible())
        inventories = {}
        for secret_hash in recipients:
            if self._stop.is_set():
                return
            with self.lock, self.auth.lock:
                device = self._eligible().get(secret_hash)
                if device is None:
                    continue
                user = self.auth.users[device["username"]]
                admin = user["role"] == "admin"
                access = (device["username"], admin, tuple(user["workspaces"]))
            if access not in inventories:
                inventory = session_inventory(
                    self.workspaces, access[0], roots, include_outside=admin
                )
                inventories[access] = {
                    session["threadId"]: (workspace_id, session)
                    for workspace_id, sessions in inventory.items()
                    if admin
                    or workspace_id != OTHER_WORKSPACE
                    and workspace_id in access[2]
                    for session in sessions
                }
            entry = inventories[access].get(thread_id)
            if entry is None:
                continue
            workspace_id, session = entry
            payload = {
                "aps": {
                    "alert": {
                        "title": f"Xedoc · {self.hostname[:128]}",
                        "body": f"{session['projectName'][:128]} — "
                        + (
                            "Input needed" if kind == "inputNeeded" else "Turn finished"
                        ),
                    },
                    "sound": "default",
                },
                "kind": kind,
                "threadId": thread_id,
                "workspaceId": workspace_id,
                "xedocHost": device.get("origin", self.origin),
                "installationId": device["installationId"],
            }
            for attempt in range(2):
                if self._stop.is_set() or attempt and self._stop.wait(1):
                    return
                with self.lock, self.auth.lock:
                    current = self._eligible().get(secret_hash)
                    if current != device:
                        break
                    user = self.auth.users[device["username"]]
                    if (
                        device["username"],
                        user["role"] == "admin",
                        tuple(user["workspaces"]),
                    ) != access:
                        break
                    try:
                        result = self.apns.send(device, payload)
                    except (
                        OSError,
                        ValueError,
                        KeyError,
                        subprocess.SubprocessError,
                        httpcore.NetworkError,
                        httpcore.ProtocolError,
                        httpcore.TimeoutException,
                    ):
                        logging.warning("APNs submission failed", exc_info=True)
                        result = "retry"
                    if result == "invalid":
                        self._save(
                            {
                                key: value
                                for key, value in self._devices.items()
                                if key != secret_hash
                            }
                        )
                    if result != "retry":
                        break

    def _run(self) -> None:
        client = None
        states = OrderedDict()
        events = deque(maxlen=2048)
        while not self._stop.is_set():
            try:
                with self.lock:
                    devices = self._eligible()
                    if devices != self._devices:
                        self._save(devices)
                if not devices:
                    if client is not None:
                        client.close()
                        client = None
                    states.clear()
                    self._wake.wait(5)
                    self._wake.clear()
                    continue
                if client is None:
                    descriptor = load_bootstrap_descriptor(
                        self.workspaces._bootstrap_path
                    )
                    client = _default_client_factory(descriptor.socket_path, 5.0)
                    client.set_notification_handler(events.append)
                    client.set_server_request_handler(lambda message: None)
                    client.initialize("xedoc-host-push", "Xedoc Host Push", "0.1.0")
                    baseline = client.request("thread/active/list", {})
                    states = OrderedDict(
                        (thread["id"], thread["status"])
                        for thread in baseline.get("data", [])[:1000]
                        if isinstance(thread, dict)
                        and isinstance(thread.get("id"), str)
                    )
                    events.clear()
                client.request("thread/active/list", {})
                while events:
                    message = events.popleft()
                    params = message.get("params", {})
                    if message.get(
                        "method"
                    ) != "thread/status/changed" or not isinstance(params, dict):
                        continue
                    thread_id, status = params.get("threadId"), params.get("status")
                    if not isinstance(thread_id, str) or not isinstance(status, dict):
                        continue
                    previous = states.pop(thread_id, {"type": "idle"})
                    states[thread_id] = status
                    while len(states) > 1000:
                        states.popitem(last=False)
                    waiting = {"waitingOnApproval", "waitingOnUserInput"}
                    needs_input = bool(
                        waiting.intersection(status.get("activeFlags", []))
                    )
                    previous_input = bool(
                        waiting.intersection(previous.get("activeFlags", []))
                    )
                    if needs_input and not previous_input:
                        self._deliver(thread_id, "inputNeeded")
                    elif previous.get("type") == "active" and status.get("type") in (
                        "idle",
                        "systemError",
                    ):
                        self._deliver(thread_id, "turnFinished")
                self._wake.wait(1)
                self._wake.clear()
            except Exception:
                logging.warning("Host push observer unavailable", exc_info=True)
                if client is not None:
                    client.close()
                    client = None
                events.clear()
                states.clear()
                self._stop.wait(5)
        if client is not None:
            client.close()
        self.apns.close()

    def close(self) -> None:
        self._stop.set()
        self._wake.set()
        self._thread.join(timeout=15)
