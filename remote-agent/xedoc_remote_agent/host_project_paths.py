"""Git worktree paths used for host project session association."""

import os
from pathlib import Path
import subprocess


def project_worktrees(directory: Path) -> list[Path]:
    if not (directory / ".git").exists():
        return [directory]
    try:
        result = subprocess.run(
            ["git", "-C", str(directory), "worktree", "list", "--porcelain", "-z"],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=2,
            check=False,
        )
        if result.returncode != 0:
            return [directory]
        records = result.stdout.split(b"\0\0")
        # A bare repository has no main working directory to group sessions under.
        if b"bare" in records[0].split(b"\0"):
            return [directory]
        paths = [
            Path(os.path.realpath(os.fsdecode(field[len(b"worktree ") :])))
            for record in records
            for field in record.split(b"\0")
            if field.startswith(b"worktree ")
        ]
        return paths if directory in paths else [directory]
    except (OSError, subprocess.TimeoutExpired, ValueError):
        return [directory]


def main_project(directory: Path) -> Path:
    if not (directory / ".git").is_file():
        return directory
    return project_worktrees(directory)[0]
