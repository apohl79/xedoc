"""Local owner-provisioned accounts for the host GUI."""

import hashlib
import hmac
import json
from pathlib import Path
import re
import secrets
import tempfile
import os
import threading
import time


class HostAuth:
    def __init__(self, home: Path) -> None:
        self.directory = home / "host-gui"
        self.path = self.directory / "users.json"
        self.session_path = self.directory / "sessions.json"
        self.lock = threading.RLock()
        self.sessions: dict[str, tuple[str, float]] = {}
        self.failures: dict[str, tuple[int, float]] = {}
        self.users = {}
        if self.path.exists():
            if self.path.stat().st_size > 1024 * 1024:
                raise ValueError("Account store exceeds size limit")
            self.users = json.loads(self.path.read_text())
        if self.session_path.exists():
            if self.session_path.stat().st_size > 128 * 1024:
                raise ValueError("Login store exceeds size limit")
            stored = json.loads(self.session_path.read_text())
            if stored.get("usersHash") == self._users_hash():
                now = time.time()
                self.sessions = {
                    key: (value[0], value[1])
                    for key, value in stored["sessions"].items()
                    if value[1] > now and value[0] in self.users
                }

    def public_users(self) -> list[dict]:
        with self.lock:
            return [
                {
                    "username": name,
                    "role": user["role"],
                    "workspaces": user["workspaces"],
                }
                for name, user in self.users.items()
            ]

    def save_user(self, name: str, role: str, workspaces: list, password: str) -> None:
        if not isinstance(name, str) or not re.fullmatch(
            r"[A-Za-z0-9_.@-]{1,64}", name
        ):
            raise ValueError("Use a username of 1–64 letters, numbers, or . _ @ -")
        if (
            role not in ("admin", "workspaceUser")
            or not isinstance(workspaces, list)
            or len(workspaces) > 256
        ):
            raise ValueError("Invalid role or workspace assignments")
        if any(not isinstance(value, str) or len(value) > 64 for value in workspaces):
            raise ValueError("Invalid workspace assignment")
        with self.lock:
            previous = self.users.get(name)
            if not previous and len(self.users) >= 256:
                raise ValueError("Account limit reached")
            if not previous or password:
                if not isinstance(password, str) or not 6 <= len(password) <= 1024:
                    raise ValueError("Passwords must contain 6–1024 characters")
                salt = secrets.token_hex(32)
                digest = self._digest(password, salt)
            else:
                salt, digest = previous["salt"], previous["digest"]
            if (
                previous
                and previous["role"] == "admin"
                and role != "admin"
                and self._admin_count() == 1
            ):
                raise ValueError("The last administrator cannot be demoted")
            updated = dict(self.users)
            updated[name] = {
                "role": role,
                "workspaces": sorted(set(workspaces)),
                "salt": salt,
                "digest": digest,
            }
            self._persist(updated)

    def delete_user(self, name: str) -> None:
        with self.lock:
            user = self.users.get(name)
            if user and user["role"] == "admin" and self._admin_count() == 1:
                raise ValueError("The last administrator cannot be deleted")
            self._persist(
                {key: value for key, value in self.users.items() if key != name}
            )

    def _admin_count(self) -> int:
        return sum(user["role"] == "admin" for user in self.users.values())

    def _persist(self, users: dict) -> None:
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(dir=self.directory)
        try:
            with os.fdopen(descriptor, "w") as stream:
                json.dump(users, stream)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        self.users = users
        self.sessions.clear()
        self._persist_sessions()

    def _users_hash(self) -> str:
        return hashlib.sha256(
            json.dumps(self.users, sort_keys=True).encode()
        ).hexdigest()

    def _persist_sessions(self) -> None:
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(dir=self.directory)
        try:
            with os.fdopen(descriptor, "w") as stream:
                json.dump(
                    {"usersHash": self._users_hash(), "sessions": self.sessions},
                    stream,
                )
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.session_path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)

    @staticmethod
    def _digest(password: str, salt: str) -> str:
        return hashlib.pbkdf2_hmac(
            "sha256", password.encode(), bytes.fromhex(salt), 600_000
        ).hex()

    def login(self, name: str, password: str, address: str) -> str:
        if (
            not isinstance(name, str)
            or not isinstance(password, str)
            or len(password) > 1024
        ):
            raise PermissionError("Invalid credentials")
        with self.lock:
            now = time.monotonic()
            self.failures = {
                key: value for key, value in self.failures.items() if value[1] > now
            }
            attempts, deadline = self.failures.get(address, (0, now + 300))
            if (
                attempts >= 10
                or len(self.failures) >= 1024
                and address not in self.failures
            ):
                raise PermissionError("Too many login attempts; try again later")
            user = self.users.get(name)
            digest = self._digest(password, user["salt"] if user else "00" * 32)
            if not user or not hmac.compare_digest(digest, user["digest"]):
                self.failures[address] = (attempts + 1, deadline)
                raise PermissionError("Invalid credentials")
            self.failures.pop(address, None)
            now = time.time()
            self.sessions = {
                key: value for key, value in self.sessions.items() if value[1] > now
            }
            if len(self.sessions) >= 256:
                self.sessions.pop(next(iter(self.sessions)))
            token = secrets.token_urlsafe(48)
            self.sessions[hashlib.sha256(token.encode()).hexdigest()] = (
                name,
                now + 7 * 24 * 3600,
            )
            self._persist_sessions()
            return token

    def account(self, token: str) -> dict:
        with self.lock:
            key = hashlib.sha256(token.encode()).hexdigest()
            name, deadline = self.sessions.get(key, ("", 0))
            if deadline <= time.time() or name not in self.users:
                self.sessions.pop(key, None)
                raise PermissionError("Login required")
            user = self.users[name]
            return {
                "username": name,
                "role": user["role"],
                "workspaces": list(user["workspaces"]),
            }

    def logout(self, token: str) -> None:
        with self.lock:
            self.sessions.pop(hashlib.sha256(token.encode()).hexdigest(), None)
            self._persist_sessions()
