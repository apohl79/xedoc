"""Per-account project pins shared by every device that signs in to the host GUI."""

import json
import os
from pathlib import Path
import tempfile
import threading


class HostPins:
    def __init__(self, home: Path) -> None:
        self.directory = home / "host-gui"
        self.path = self.directory / "pins.json"
        self.lock = threading.Lock()
        self.pins: dict[str, dict[str, list[str]]] = {}
        if self.path.exists():
            if self.path.stat().st_size > 1024 * 1024:
                raise ValueError("Pin store exceeds size limit")
            self.pins = json.loads(self.path.read_text())

    def get(self, username: str, workspace_id: str) -> list[str]:
        with self.lock:
            return list(self.pins.get(username, {}).get(workspace_id, []))

    def set(
        self, username: str, workspace_id: str, project_name: str, pinned: bool
    ) -> list[str]:
        if not isinstance(project_name, str) or not 1 <= len(project_name) <= 255:
            raise ValueError("Invalid project name")
        if not isinstance(pinned, bool):
            raise ValueError("pinned must be a boolean")
        with self.lock:
            workspaces = dict(self.pins.get(username, {}))
            names = [name for name in workspaces.get(workspace_id, []) if name != project_name]
            if pinned:
                names.append(project_name)
            if len(names) > 256 or (
                workspace_id not in workspaces and len(workspaces) >= 256
            ):
                raise ValueError("Pin limit reached")
            if names:
                workspaces[workspace_id] = names
            else:
                workspaces.pop(workspace_id, None)
            updated = dict(self.pins)
            if workspaces:
                updated[username] = workspaces
            else:
                updated.pop(username, None)
            self._persist(updated)
            return list(names)

    def remove_user(self, username: str) -> None:
        with self.lock:
            if username in self.pins:
                self._persist(
                    {key: value for key, value in self.pins.items() if key != username}
                )

    def _persist(self, pins: dict) -> None:
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(dir=self.directory)
        try:
            with os.fdopen(descriptor, "w") as stream:
                json.dump(pins, stream)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(temporary, self.path)
        finally:
            if os.path.exists(temporary):
                os.unlink(temporary)
        self.pins = pins
